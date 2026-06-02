//! A minimal x86-64 / ELF code-generation backend (Linux).
//!
//! A second native target alongside the [AArch64 backend](super::arm64). It
//! hand-emits x86-64 machine code and writes a **self-contained static ELF
//! executable** — its own `_start` runs the program and calls the `exit` syscall,
//! so there is no linker, no libc, and no relocations. The interpreter remains
//! the conformance oracle.
//!
//! Implemented: top-level code and **functions** (recursion, up to six integer/
//! pointer args via the System V registers), **locals** (a `rbp` frame,
//! `[rbp - off]`), **control flow** (`if`/`else`, `while`, `for`, `do`, `break`,
//! `continue`, `switch`/`case`/`default` incl. `lo ... hi` ranges and the
//! `start:`/`end:` sub-labels via a compare-chain, and `goto`/labels),
//! **comparisons** and short-circuit `&&`/`||`/`!`, the
//! unary `- + ~ ++ --`, the binary `+ - * / % & | ^ << >>` (with `>>`/`/`/`%`
//! and the relational operators **signedness-directed** — `sar`/`idiv` vs
//! `shr`/`div`, signed vs unsigned condition codes), **pointers & arrays**
//! (`&x`, `*p`, `a[i]` incl. 2-D, pointer arithmetic (pointee-scaled) and
//! difference, width-aware narrow loads/stores via `movsx`/`movzx`, array
//! parameters that decay to a by-reference pointer), **classes & unions**
//! (`repr(C)` layout from the shared `layout` pass: member access `a.x` and
//! `p->x`, nested fields, `sizeof`, whole-class assignment, **by-value**
//! parameters as a callee-side `rep movsb` copy, **by-value returns** (sret: the
//! caller hands a result-temp pointer to the callee in r11), member access on a
//! class-returning call (`Mk().x`), arrays of classes, and union aliasing —
//! anonymous-embedded unions resolve through the promoted offset),
//! **globals** (top-level variables live in a zero-filled BSS region and are
//! reachable from any function), **F64** (SSE2: `xmm0`/`xmm1` as the float
//! result/temp, args in `xmm0..xmm7` and returns in `xmm0`; arithmetic, `-`,
//! comparisons, int↔float conversions (signedness-directed), literals, locals,
//! globals, arrays, params/returns), **printing** — a bare string prints
//! verbatim, `"fmt", …` and `Print("fmt", …)` are printf-style with the full
//! `%[flags][width][.prec]conv` grammar for `%d %i %u %x %X %o %c %s %%`
//! (flags `-0+ #`, `*` width/precision), **`%f`/`%e`/`%g` float printing** —
//! correctly-rounded via a bignum (matches Rust's `{:.P}`/`{:.Pe}` byte-for-byte,
//! including round-half-to-even ties) — and a slice of the **core-library
//! builtins**, lowered with no libc: `MAlloc`/`Free` (an `mmap`-backed bump
//! allocator), the string/memory ops (`StrLen`/`StrCmp`/`StrCpy`/`StrCat`/…,
//! `MemCpy`/`MemMove`/`MemSet`/`MemCmp`/…) as emitted routines, `ToUpper`/
//! `ToLower`/`Abs`/`Sign`, `RandU64` (splitmix64), SSE `Sqrt`/`Fabs`, and the
//! **sprintf family** (`StrPrint`/`CatPrint`/`I64ToStr` — printf into a buffer via
//! an output sink, see below). The transcendental math builtins
//! (`Sin`/`Cos`/`Pow`/`Exp`/`Ln`/…) are **deliberately absent**: a freestanding
//! static ELF has no libm, and — like Rust's `core` (which omits them; they live
//! in `std` for the platform libm) — we don't fake them with approximations,
//! since libm results aren't bit-identical across platforms anyway (IEEE 754
//! doesn't require correctly-rounded transcendentals). Otherwise the core-library
//! builtins are complete here. Anything unimplemented is a build-time error.
//!
//! Expression evaluation is a stack machine: a value lands in `rax` (or `xmm0`
//! for an F64-typed expression), a binary op's left operand is spilled to the
//! machine stack while the right is computed (so values survive nested calls);
//! an lvalue's address is computed by `gen_addr`, with width-aware load/store
//! through it. Printing needs no libc: a
//! tiny emitted runtime (`fmt_int`/`fmt_str`, mirroring [`crate::fmt`]) formats
//! integers/strings into a BSS scratch buffer and hands them to a single output
//! sink (`out_write`) — which goes to the `write` syscall when the `out_ptr`
//! global is 0, or appends to a destination buffer otherwise, so the same format
//! machinery serves both `Print` and `StrPrint`/`CatPrint` — and `MStrPrint`,
//! whose buffer the sink *grows* (reallocs) on overflow, like libc `vasprintf`.
//! String literals live
//! after the code (RIP-relative addressed). ELF layout:
//! `[ELF header | one PT_LOAD | code | strings | BSS]`, mapped R+W+X at
//! `0x400000` (`p_memsz > p_filesz` reserves the zero-filled BSS), `_start` first.

use std::collections::HashMap;

use crate::ast::*;
use crate::codegen::CodegenError;
use crate::layout::{self, Layouts};
use crate::token::Pos;

mod asm;
mod linux;
mod windows;

use asm::Asm;
pub use linux::X64Linux;
pub use windows::X64Windows;

/// The OS-specific policy for the shared x86-64 code generator — the handful of
/// points where the emitted program touches the operating system. Everything
/// else in [`Cg`]/[`Asm`] is OS-agnostic (the same instruction set). The Linux
/// target ([`X64Linux`]) uses raw syscalls and a freestanding static ELF; the
/// Windows target ([`X64Windows`]) calls `kernel32` imports from a self-contained
/// PE. Each seam is a small instruction sequence with a fixed register contract
/// so the shared `Cg` can drive it without knowing the OS.
trait OsTarget {
    /// Emit process exit. The exit status is in `rax` (its low 32 bits).
    fn emit_exit(&mut self, asm: &mut Asm);

    /// Emit "allocate `rsi` zeroed, page-aligned bytes; base address → `rax`;
    /// `rsi` preserved" — the fresh-chunk grab inside the `MAlloc` bump allocator.
    fn emit_page_alloc(&mut self, asm: &mut Asm);

    /// Emit "write the `rdx` bytes at `rsi` to standard output" — the terminal
    /// case of the formatted-output sink.
    fn emit_write_stdout(&mut self, asm: &mut Asm);

    /// Read the wall clock into `rax` as nanoseconds since the Unix epoch.
    /// `scratch` is a 16-byte BSS slot for the OS time structure. Linux uses
    /// `clock_gettime(CLOCK_REALTIME)`; Windows `GetSystemTimePreciseAsFileTime`.
    fn emit_unix_ns(&mut self, asm: &mut Asm, scratch: i32);

    /// Read the monotonic clock into `rax` as nanoseconds (unspecified origin).
    /// Linux `clock_gettime(CLOCK_MONOTONIC)`; Windows `GetTickCount64`.
    fn emit_mono_ns(&mut self, asm: &mut Asm, scratch: i32);

    /// Suspend the thread for the nanosecond count in `rax`. Linux `nanosleep`,
    /// Windows `Sleep` (millisecond granularity).
    fn emit_sleep(&mut self, asm: &mut Asm, scratch: i32);

    /// Emit the entry preamble that captures the command line into the BSS slots
    /// at `argc_off` / `argv_off` (argv there is a pointer to the argv array).
    /// Runs just after the entry's frame is set up, so the frame pointer `rbp` is
    /// valid. Only emitted when the program uses `ArgC`/`ArgV`. Linux reads them
    /// off the initial stack; Windows builds them from `GetCommandLineA`.
    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32);

    /// Package the emitted program into a runnable executable. Takes ownership of
    /// the `Asm` so a policy can read its layout (and, on Windows, append an import
    /// table) before calling [`Asm::finish`]; `bss` is the zero-filled BSS that
    /// follows the image in memory. Linux finishes with no imports and wraps the
    /// blob in an ELF; Windows builds a kernel32 import table, finishes with it,
    /// and wraps the blob in a PE.
    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError>;
}

/// Compile a type-checked program to a runnable executable image. This driver is
/// **OS-independent** — it lays out functions and globals, emits the entry, the
/// function bodies, and the print/builtin runtime, then resolves fixups — and
/// defers the four OS-specific steps (exit, page allocation, the stdout sink, and
/// the container format) to `os`. The concrete targets ([`X64Linux`],
/// [`X64Windows`]) just pick the policy and write the returned bytes to disk.
fn compile(program: &Program, os: Box<dyn OsTarget>) -> Result<Vec<u8>, CodegenError> {
    let (layouts, _) = layout::compute(program);
    let mut cg = Cg::new(layouts, os);

    // Pre-assign a label to every defined function (so calls can forward-ref).
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if f.body.is_some() {
                let label = cg.asm.new_label();
                cg.funcs.insert(f.name.clone(), label);
                let params = f.params.iter().map(|p| p.ty.clone()).collect();
                cg.funcs_sig.insert(f.name.clone(), (params, f.ret.clone()));
                cg.funcs_va.insert(f.name.clone(), f.varargs);
            }
        }
    }

    // Top-level variable declarations are globals (BSS-allocated, accessible
    // from any function); collect them before emitting any code.
    for item in &program.items {
        if let StmtKind::VarDecl { decls } = &item.kind {
            for d in decls {
                cg.declare_global(&d.name, d.ty.clone());
            }
        }
    }

    // If the program reads command-line args, reserve the argc/argv BSS slots the
    // entry will populate; arg-free programs are left byte-for-byte unchanged.
    if crate::ast::program_calls_any(program, &["ArgC", "ArgV"]) {
        cg.uses_args = true;
        cg.argc_off = Some(cg.alloc_bss(8, 8));
        cg.argv_off = Some(cg.alloc_bss(8, 8));
    }

    // The entry runs the top-level statements; a top-level `return` exits with it.
    cg.emit_main(&program.items)?;

    // Then each function body.
    for item in &program.items {
        if let StmtKind::Func(f) = &item.kind {
            if let Some(body) = &f.body {
                let label = cg.funcs[&f.name];
                cg.emit_function(label, &f.params, &f.ret, body, f.varargs)?;
            }
        }
    }

    // The tiny print runtime (only the helpers actually used).
    cg.emit_helpers();
    // Builtin runtime routines (MAlloc, string/mem ops, …), only those used.
    cg.emit_rt_routines();

    let bss = cg.bss as u64;
    cg.os.wrap(cg.asm, bss)
}

// ---- code generation ----

/// Number of System V integer argument registers (`gen_call` / parameter setup).
const ARG_REGS: usize = 6;

/// A local's frame location. Its lowest byte is at `[rbp - off]`. An array
/// parameter decays to a pointer, so its slot holds the *address* of the caller's
/// data (`indirect`) rather than the data itself.
#[derive(Clone)]
struct VarLoc {
    off: i32,
    ty: Type,
    indirect: bool,
}

/// A routine in the tiny print runtime (emitted once, on demand).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Helper {
    /// Format an integer with full flags/width/precision (mirrors `fmt::render_int`).
    /// `rdi`=value, `rsi`=radix, `rdx`=flags, `rcx`=width, `r8`=precision.
    FmtInt,
    /// Format a string/char with width/precision (mirrors `fmt::render_str`).
    /// `rdi`=ptr, `rsi`=len (−1 ⇒ strlen), `rdx`=flags, `rcx`=width, `r8`=precision.
    FmtStr,
    /// The output sink: `rsi`=buf, `rdx`=len. Writes to stdout when the `out_ptr`
    /// global is 0, else appends to `[out_ptr]` (advancing it) — the StrPrint path.
    OutWrite,
    /// Grow the owned `MStrPrint` buffer so the pending `rdx`-byte write (plus a
    /// trailing NUL) fits, reallocating + copying. Preserves `rsi`/`rdx`, returns
    /// the new cursor in `rax`, and updates `out_base`/`out_ptr`/`out_limit`.
    GrowSink,
    /// Format an F64 as `%f`. `xmm0`=value, `edi`=precision, `esi`=flags, `edx`=width.
    FmtFloat,
    /// Format an F64 as `%e`/`%g`. Same registers; `ecx`=conv (1 `e`, 2 `g`;
    /// bit 2 = uppercase). Works from the value's exact decimal expansion.
    FmtFloatEg,
    /// Bignum scratch ops on the shared `BIGNUM` array: `*= rdi`.
    BnMul,
    /// Bignum `>>= rdi` with round-half-even.
    BnShr,
    /// Bignum `<<= rdi`.
    BnShl,
    /// Bignum `/= 10`, returning the remainder digit in rax.
    BnDiv10,
}

/// Number of 64-bit limbs in the float-printing bignum. Must hold a double's full
/// exact decimal expansion: the smallest subnormal's `m·5^1074` is ~767 digits ≈
/// 2548 bits, so 48 limbs (3072 bits ≈ 925 digits) covers it with margin.
const NLIMBS: i32 = 48;
// Freestanding printf scratch sizes (≥ the clamped fmt::MAX_WIDTH/MAX_PRECISION),
// so the formatters can never overflow their fixed buffers.
const FS_INT_DIGBUF: i32 = 600;
const FS_OUTBUF: i32 = 1152;
const FS_FLOAT_DIGBUF: i32 = 1024;
const FS_SIGBUF: i32 = 576;

// Register numbers for the generic encoders.
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R10: u8 = 10;
const R11: u8 = 11;
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
const R15: u8 = 15;

// `fmt_int`/`fmt_str` flag bits (compile-time assembled from a parsed `fmt::Spec`).
const F_SIGNED: i32 = 1; // a signed conversion (`%d`/`%i`): emit a sign, magnitude in digits
const F_UPPER: i32 = 2; // uppercase hex (`%X`) and `0X` prefix
const F_MINUS: i32 = 4; // left-justify
const F_ZERO: i32 = 8; // zero-pad
const F_PLUS: i32 = 16; // always show a sign
const F_SPACE: i32 = 32; // space before a non-negative
const F_HASH: i32 = 64; // alternate form (`0x`/leading `0`)

/// A global variable's location: a byte offset into the BSS region. Its address
/// is RIP-relative (resolved like a string reference, but past the file image).
#[derive(Clone)]
struct GlobalLoc {
    off: i32,
    ty: Type,
}

struct Cg {
    asm: Asm,
    os: Box<dyn OsTarget>, // the per-OS policy: exit, page alloc, stdout sink, container
    layouts: Layouts,
    scopes: Vec<HashMap<String, VarLoc>>,
    depth: i32,                    // bytes of locals allocated below rbp
    funcs: HashMap<String, usize>, // name -> code label
    globals: HashMap<String, GlobalLoc>,
    bss: i32, // total bytes of BSS allocated (globals + print scratch)
    digbuf: Option<i32>,
    outbuf: Option<i32>,
    charbuf: Option<i32>,
    break_targets: Vec<usize>,
    continue_targets: Vec<usize>,
    ret_label: usize, // `return` jumps here: the epilogue (function) or exit (main)
    cur_ret: Type,    // return type of the function being emitted (drives F64 returns)
    sret_off: Option<i32>, // frame slot holding the sret pointer (a class-returning fn)
    labels: HashMap<String, usize>, // named `goto` labels in the current function
    funcs_sig: HashMap<String, (Vec<Type>, Type)>, // name -> (param types, return type)
    funcs_va: HashMap<String, bool>, // name -> is variadic (`...`)
    /// For a variadic function being emitted: frame offsets of the two hidden
    /// params the caller appends — `(va_ptr_off, va_cnt_off)` — read by `VarArg*`.
    cur_va: Option<(i32, i32)>,
    helpers: HashMap<Helper, usize>, // print runtime routine -> code label (on first use)
    rt_routines: HashMap<&'static str, usize>, // builtin runtime routine -> label (on first use)
    out_ptr_off: Option<i32>,        // BSS slot: the formatted-output sink pointer (0 = stdout)
    // Growing-sink globals (set only when a program uses `MStrPrint`): the base of
    // the owned, reallocating buffer and its current capacity end. `out_limit != 0`
    // is what tells the sink it may grow rather than blindly append (the StrPrint
    // path leaves it 0). `out_limit_off.is_some()` gates the grow branch in the sink.
    out_base_off: Option<i32>,
    out_limit_off: Option<i32>,
    bignum_off: Option<i32>, // BSS offset of the float-printing bignum (NLIMBS limbs)
    // Command-line args: BSS slots holding argc and the argv array pointer, set
    // when the program uses `ArgC`/`ArgV` (the entry captures them). `None`/false
    // for arg-free programs, which are then byte-for-byte unchanged.
    argc_off: Option<i32>,
    argv_off: Option<i32>,
    uses_args: bool,
}

impl Cg {
    fn new(layouts: Layouts, os: Box<dyn OsTarget>) -> Self {
        Cg {
            asm: Asm::new(),
            os,
            layouts,
            scopes: Vec::new(),
            depth: 0,
            funcs: HashMap::new(),
            globals: HashMap::new(),
            bss: 0,
            digbuf: None,
            outbuf: None,
            charbuf: None,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            ret_label: 0,
            cur_ret: Type::I64,
            funcs_va: HashMap::new(),
            cur_va: None,
            sret_off: None,
            labels: HashMap::new(),
            funcs_sig: HashMap::new(),
            helpers: HashMap::new(),
            rt_routines: HashMap::new(),
            out_ptr_off: None,
            out_base_off: None,
            out_limit_off: None,
            bignum_off: None,
            argc_off: None,
            argv_off: None,
            uses_args: false,
        }
    }

    /// BSS offset of the float-printing bignum (`NLIMBS` 64-bit limbs), on first use.
    fn bignum_global(&mut self) -> i32 {
        if let Some(o) = self.bignum_off {
            return o;
        }
        let o = self.alloc_bss(NLIMBS * 8, 8);
        self.bignum_off = Some(o);
        o
    }

    /// BSS offset of the formatted-output sink pointer (`out_ptr`), allocated on
    /// first use. 0 means "write to stdout"; otherwise it is the current write
    /// position in a `StrPrint`/`CatPrint` destination buffer.
    fn out_ptr_global(&mut self) -> i32 {
        if let Some(o) = self.out_ptr_off {
            return o;
        }
        let o = self.alloc_bss(8, 8);
        self.out_ptr_off = Some(o);
        o
    }

    /// The code label for a builtin runtime routine, assigning one on first use.
    fn rt_routine(&mut self, name: &'static str) -> usize {
        if let Some(&l) = self.rt_routines.get(name) {
            return l;
        }
        let l = self.asm.new_label();
        self.rt_routines.insert(name, l);
        l
    }

    /// Reserve `size` bytes of zero-initialised BSS at `align`, returning the
    /// offset of its first byte (its address is the BSS base + this offset).
    fn alloc_bss(&mut self, size: i32, align: i32) -> i32 {
        let a = align.max(1);
        let off = (self.bss + a - 1) / a * a;
        self.bss = off + size.max(1);
        off
    }

    /// Register a top-level variable as a global in the BSS region.
    fn declare_global(&mut self, name: &str, ty: Type) {
        let off = self.alloc_bss(self.size_of(&ty), self.align_of(&ty));
        self.globals.insert(name.to_string(), GlobalLoc { off, ty });
    }

    /// Offsets of the print-runtime scratch buffers (allocated on first use):
    /// `digbuf` builds digit strings, `outbuf` assembles a padded field, and
    /// `charbuf` holds a single `%c` byte.
    fn digbuf(&mut self) -> i32 {
        if let Some(o) = self.digbuf {
            return o;
        }
        let o = self.alloc_bss(FS_INT_DIGBUF, 1);
        self.digbuf = Some(o);
        o
    }
    fn outbuf(&mut self) -> i32 {
        if let Some(o) = self.outbuf {
            return o;
        }
        let o = self.alloc_bss(FS_OUTBUF, 1);
        self.outbuf = Some(o);
        o
    }
    fn charbuf(&mut self) -> i32 {
        if let Some(o) = self.charbuf {
            return o;
        }
        let o = self.alloc_bss(1, 1);
        self.charbuf = Some(o);
        o
    }
    /// BSS slots for the growing sink (allocated together on first `MStrPrint`):
    /// `out_base` is the start of the owned, reallocating output buffer (and the
    /// value `MStrPrint` ultimately returns); `out_limit` is one past its last
    /// usable byte. A nonzero `out_limit` is the sink's signal that it owns the
    /// buffer and may grow it; `StrPrint` never sets it, so it keeps appending.
    fn out_base_global(&mut self) -> i32 {
        if let Some(o) = self.out_base_off {
            return o;
        }
        let o = self.alloc_bss(8, 8);
        self.out_base_off = Some(o);
        o
    }
    fn out_limit_global(&mut self) -> i32 {
        if let Some(o) = self.out_limit_off {
            return o;
        }
        let o = self.alloc_bss(8, 8);
        self.out_limit_off = Some(o);
        o
    }

    /// The code label for a print helper, assigning one on first use.
    fn helper(&mut self, h: Helper) -> usize {
        if let Some(&l) = self.helpers.get(&h) {
            return l;
        }
        let l = self.asm.new_label();
        self.helpers.insert(h, l);
        l
    }

    fn size_of(&self, ty: &Type) -> i32 {
        self.layouts.size_of(ty) as i32
    }
    fn align_of(&self, ty: &Type) -> i32 {
        self.layouts.align_of(ty) as i32
    }
    fn stride_of(&self, ty: &Type) -> i32 {
        self.layouts.stride_of(ty) as i32
    }
    fn expr_ty(&self, e: &Expr) -> Type {
        e.ty().unwrap_or(Type::I64)
    }

    /// Reserve `size` aligned bytes below rbp; the slot's lowest byte is at
    /// `[rbp - off]`.
    fn alloc(&mut self, size: i32, align: i32) -> i32 {
        let a = align.max(1);
        let total = self.depth + size;
        self.depth = (total + a - 1) / a * a; // round up to `a`
        self.depth
    }
    fn declare(&mut self, name: &str, ty: Type, indirect: bool) -> i32 {
        let (size, align) = if indirect {
            (8, 8) // an array parameter is really a pointer
        } else {
            (self.size_of(&ty).max(1), self.align_of(&ty).max(1))
        };
        let off = self.alloc(size, align);
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), VarLoc { off, ty, indirect });
        off
    }
    fn lookup(&self, name: &str) -> Option<VarLoc> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    fn emit_main(&mut self, items: &[Stmt]) -> Result<(), CodegenError> {
        self.begin_function();
        self.cur_ret = Type::I64; // a top-level `return` exits with an int
        self.collect_labels(items);
        let frame = self.asm.prologue();
        // Capture the command line for `ArgC`/`ArgV` once the frame is set up.
        if self.uses_args {
            let (c, v) = (self.argc_off.unwrap(), self.argv_off.unwrap());
            self.os.emit_capture_args(&mut self.asm, c, v);
        }
        for item in items {
            match &item.kind {
                // Function/type definitions aren't statements that run here.
                StmtKind::Func(_) | StmtKind::Class(_) => {}
                // A top-level declaration is a global: its storage is in BSS
                // (already zeroed); only its initialiser runs, in program order.
                StmtKind::VarDecl { decls } => {
                    for d in decls {
                        if let Some(init) = &d.init {
                            self.gen_global_init(d, init)?;
                        }
                    }
                }
                _ => self.gen_stmt(item)?,
            }
        }
        self.asm.mov_rax_imm(0); // default exit status if no `return` was hit
        self.asm.place(self.ret_label);
        self.os.emit_exit(&mut self.asm); // exit status in rax
        self.asm.patch_frame(frame, align16(self.depth));
        Ok(())
    }

    fn emit_function(
        &mut self,
        label: usize,
        params: &[Param],
        ret: &Type,
        body: &[Stmt],
        varargs: bool,
    ) -> Result<(), CodegenError> {
        self.begin_function();
        self.cur_ret = ret.clone();
        self.cur_va = None;
        self.collect_labels(body);
        self.asm.place(label);
        let frame = self.asm.prologue();
        // A class-returning function received its result buffer's address in r11;
        // save it to a frame slot before any code can clobber r11.
        if matches!(ret, Type::Named(_)) {
            let off = self.alloc(8, 8);
            self.asm.mov_rr(RAX, R11);
            self.asm.store_local(off, 8);
            self.sret_off = Some(off);
        }
        // System V classing: integer/pointer/class-pointer args in rdi.., F64 args
        // in xmm0.. — the two classes counted independently. Pass 1 spills every
        // incoming argument register to a slot using only `rax` (no `rep movsb`,
        // which would clobber later args still in rsi/rdi/rcx); pass 2 then copies
        // each by-value class from the saved pointer into its local slot.
        let mut gpr = 0usize;
        let mut fpr = 0usize;
        let mut class_copies: Vec<(i32, i32, i32)> = Vec::new(); // (dest slot, ptr temp, size)
        for p in params {
            let name = p.name.as_deref().unwrap_or("_");
            if is_f64(&p.ty) {
                if fpr >= 8 {
                    return Err(CodegenError::at(
                        p.span.pos,
                        "x86_64 backend: at most 8 floating-point parameters",
                    ));
                }
                let off = self.declare(name, p.ty.clone(), false);
                self.asm.movsd_store_local_xmm(off, fpr as u8); // slot = xmmN
                fpr += 1;
                continue;
            }
            if gpr >= ARG_REGS {
                return Err(CodegenError::at(
                    p.span.pos,
                    "x86_64 backend: at most 6 integer parameters",
                ));
            }
            if matches!(p.ty, Type::Named(_)) {
                // A class is passed by value as a pointer; stash the pointer now
                // (only `rax` touched) and copy it in pass 2.
                let off = self.declare(name, p.ty.clone(), false);
                let ptr_tmp = self.alloc(8, 8);
                self.asm.mov_rax_argreg(gpr);
                self.asm.store_local(ptr_tmp, 8);
                class_copies.push((off, ptr_tmp, self.size_of(&p.ty)));
                gpr += 1;
                continue;
            }
            // An array parameter decays to a pointer (passed by reference); its
            // slot holds the incoming pointer (8 bytes), marked indirect.
            let indirect = matches!(p.ty, Type::Array(..));
            let off = self.declare(name, p.ty.clone(), indirect);
            self.asm.mov_rax_argreg(gpr);
            self.asm.store_local(off, 8); // a register-passed arg is 8 bytes wide
            gpr += 1;
        }
        for (dest, ptr_tmp, size) in class_copies {
            self.asm.load_local(ptr_tmp, 8, false); // rax = source pointer
            self.asm.mov_rsi_rax(); // rsi = src
            self.asm.lea_local(dest);
            self.asm.mov_rdi_rax(); // rdi = dest slot
            self.asm.mov_rcx_imm32(size);
            self.asm.rep_movsb();
        }
        // A variadic function takes two hidden integer params after the named ones
        // (in argreg[gpr], argreg[gpr+1]): the caller's vararg buffer pointer and
        // the count. Spill them to slots for `VarArg*` to read.
        if varargs {
            if gpr + 1 >= ARG_REGS {
                return Err(CodegenError::at(
                    Pos::new(0, 0),
                    "x86_64 backend: too many named params before `...`",
                ));
            }
            let ptr_off = self.alloc(8, 8);
            self.asm.mov_rax_argreg(gpr);
            self.asm.store_local(ptr_off, 8);
            let cnt_off = self.alloc(8, 8);
            self.asm.mov_rax_argreg(gpr + 1);
            self.asm.store_local(cnt_off, 8);
            self.cur_va = Some((ptr_off, cnt_off));
        }
        for s in body {
            self.gen_stmt(s)?;
        }
        // Default return (U0 / a missing `return`): 0, or 0.0 for an F64 function.
        if is_f64(ret) {
            self.asm.mov_rax_imm(0);
            self.asm.movq_xmm_from_r(0, RAX);
        } else {
            self.asm.mov_rax_imm(0);
        }
        self.asm.place(self.ret_label);
        self.asm.epilogue();
        self.asm.patch_frame(frame, align16(self.depth));
        Ok(())
    }

    fn begin_function(&mut self) {
        self.scopes = vec![HashMap::new()];
        self.depth = 0;
        self.break_targets.clear();
        self.continue_targets.clear();
        self.ret_label = self.asm.new_label();
        self.sret_off = None;
        self.labels.clear();
    }

    /// Pre-assign a code label to every named `goto` target in `body`, so a
    /// `goto` can reference a label declared later (forward jump).
    fn collect_labels(&mut self, body: &[Stmt]) {
        for s in body {
            self.collect_labels_stmt(s);
        }
    }
    fn collect_labels_stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Label(name) => {
                let id = self.asm.new_label();
                self.labels.insert(name.clone(), id);
            }
            StmtKind::Block(b) => self.collect_labels(b),
            StmtKind::If { then, else_, .. } => {
                self.collect_labels_stmt(then);
                if let Some(e) = else_ {
                    self.collect_labels_stmt(e);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::DoWhile { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::Switch { body, .. } => self.collect_labels_stmt(body),
            _ => {}
        }
    }

    // ---- statements ----

    fn gen_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
        match &s.kind {
            StmtKind::Empty | StmtKind::Include(_) | StmtKind::Class(_) => {}
            StmtKind::Block(stmts) => {
                self.scopes.push(HashMap::new());
                for st in stmts {
                    self.gen_stmt(st)?;
                }
                self.scopes.pop();
            }
            StmtKind::Expr(e) => {
                // A bare string prints itself *verbatim* (no `%` processing);
                // `"fmt", a, b` and `Print("fmt", …)` are printf-style. Everything
                // else is evaluated for effect.
                if let ExprKind::Str(lit) = &e.kind {
                    self.emit_literal(lit.as_bytes());
                } else if let Some((fmt, args)) = as_print(e) {
                    self.gen_print(&fmt, args, s.span.pos)?;
                } else {
                    self.gen_expr(e)?;
                }
            }
            StmtKind::VarDecl { decls } => {
                for d in decls {
                    if is_f64(&d.ty) {
                        let off = self.declare(&d.name, d.ty.clone(), false);
                        match &d.init {
                            Some(init) => self.gen_foperand(init)?, // xmm0 = value
                            None => {
                                self.asm.mov_rax_imm(0); // 0.0 has all-zero bits
                                self.asm.movq_xmm_from_r(0, RAX);
                            }
                        }
                        self.asm.movsd_store_local(off);
                    } else if is_scalar(&d.ty) {
                        let off = self.declare(&d.name, d.ty.clone(), false);
                        let size = self.size_of(&d.ty);
                        match &d.init {
                            Some(init) => self.gen_int_expr(init, &d.ty)?,
                            None => self.asm.mov_rax_imm(0),
                        }
                        self.asm.store_local(off, size);
                    } else if is_aggregate(&d.ty) {
                        let off = self.declare(&d.name, d.ty.clone(), false);
                        let size = self.size_of(&d.ty);
                        match &d.init {
                            None => self.gen_zero(off, size), // uninit aggregates read as 0
                            // Copy-initialise a class from another class value.
                            Some(init)
                                if matches!(d.ty, Type::Named(_)) && !is_brace_init(init) =>
                            {
                                self.gen_expr(init)?; // rax = source address
                                self.gen_memcpy_to_local(off, size);
                            }
                            Some(_) => {
                                return Err(CodegenError::at(
                                    d.span.pos,
                                    "x86_64 backend: array/brace initializers are unsupported yet",
                                ));
                            }
                        }
                    } else {
                        return Err(CodegenError::at(
                            d.span.pos,
                            "x86_64 backend: only scalar/array/class locals are supported yet",
                        ));
                    }
                }
            }
            StmtKind::Return(val) => {
                let ret = self.cur_ret.clone();
                if matches!(ret, Type::Named(_)) {
                    // Copy the class result through the saved sret pointer.
                    if let Some(e) = val {
                        let off = self.sret_off.expect("class return needs an sret slot");
                        let size = self.size_of(&ret);
                        self.gen_expr(e)?; // rax = source address
                        self.asm.mov_rsi_rax(); // rsi = src
                        self.asm.load_local(off, 8, false); // rax = sret pointer
                        self.asm.mov_rdi_rax(); // rdi = dest
                        self.asm.mov_rcx_imm32(size);
                        self.asm.rep_movsb();
                    }
                } else if is_f64(&ret) {
                    match val {
                        Some(e) => self.gen_foperand(e)?, // xmm0 = result
                        None => {
                            self.asm.mov_rax_imm(0);
                            self.asm.movq_xmm_from_r(0, RAX);
                        }
                    }
                } else {
                    match val {
                        Some(e) => self.gen_int_expr(e, &ret)?,
                        None => self.asm.mov_rax_imm(0),
                    }
                }
                self.asm.jmp(self.ret_label);
            }
            StmtKind::If { cond, then, else_ } => {
                let l_else = self.asm.new_label();
                self.gen_cond_jump_false(cond, l_else)?;
                self.gen_stmt(then)?;
                if let Some(else_branch) = else_ {
                    let l_end = self.asm.new_label();
                    self.asm.jmp(l_end);
                    self.asm.place(l_else);
                    self.gen_stmt(else_branch)?;
                    self.asm.place(l_end);
                } else {
                    self.asm.place(l_else);
                }
            }
            StmtKind::While { cond, body } => {
                let l_top = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                self.gen_cond_jump_false(cond, l_end)?;
                self.break_targets.push(l_end);
                self.continue_targets.push(l_top);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.jmp(l_top);
                self.asm.place(l_end);
            }
            StmtKind::DoWhile { body, cond } => {
                let l_top = self.asm.new_label();
                let l_cont = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                self.break_targets.push(l_end);
                self.continue_targets.push(l_cont);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.place(l_cont);
                self.gen_expr(cond)?;
                self.asm.test_rax();
                self.asm.jne(l_top); // loop while cond is non-zero
                self.asm.place(l_end);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.gen_stmt(init)?;
                }
                let l_top = self.asm.new_label();
                let l_cont = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.asm.place(l_top);
                if let Some(cond) = cond {
                    self.gen_cond_jump_false(cond, l_end)?;
                }
                self.break_targets.push(l_end);
                self.continue_targets.push(l_cont);
                self.gen_stmt(body)?;
                self.break_targets.pop();
                self.continue_targets.pop();
                self.asm.place(l_cont);
                if let Some(step) = step {
                    self.gen_expr(step)?;
                }
                self.asm.jmp(l_top);
                self.asm.place(l_end);
                self.scopes.pop();
            }
            StmtKind::Break => {
                let l = *self.break_targets.last().ok_or_else(|| {
                    CodegenError::at(s.span.pos, "x86_64 backend: `break` outside a loop")
                })?;
                self.asm.jmp(l);
            }
            StmtKind::Continue => {
                let l = *self.continue_targets.last().ok_or_else(|| {
                    CodegenError::at(s.span.pos, "x86_64 backend: `continue` outside a loop")
                })?;
                self.asm.jmp(l);
            }
            StmtKind::Switch { cond, body } => self.gen_switch(cond, body, s.span.pos)?,
            // case/default/start/end markers are placed by gen_switch, not run here.
            StmtKind::Case { .. }
            | StmtKind::Default
            | StmtKind::SwitchStart
            | StmtKind::SwitchEnd => {}
            StmtKind::Label(name) => {
                let id = self.labels[name];
                self.asm.place(id);
            }
            StmtKind::Goto(name) => {
                let id = *self.labels.get(name).ok_or_else(|| {
                    CodegenError::at(
                        s.span.pos,
                        format!("x86_64 backend: unknown label `{name}`"),
                    )
                })?;
                self.asm.jmp(id);
            }
            other => {
                return Err(CodegenError::at(
                    s.span.pos,
                    format!("x86_64 backend: unsupported statement {other:?}"),
                ));
            }
        }
        Ok(())
    }

    /// Lower a `switch`. Evaluates the discriminant once into a frame slot, then a
    /// linear compare-chain dispatches to each `case` label (single value or a
    /// `lo ... hi` range), falling through to `default`/the epilogue/the exit when
    /// nothing matches. The optional `start:`/`end:` sub-labels partition the body
    /// into a prologue (runs before dispatch) and epilogue (reached by fall-through,
    /// skipped by `break`). Mirrors the arm64 backend's compare-chain.
    fn gen_switch(&mut self, cond: &Expr, body: &Stmt, pos: Pos) -> Result<(), CodegenError> {
        let StmtKind::Block(stmts) = &body.kind else {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: switch body must be a block",
            ));
        };

        self.gen_expr(cond)?; // rax = switch value
        let voff = self.alloc(8, 8);
        self.asm.store_local(voff, 8);

        let start_idx = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchStart));
        let first_case = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::Case { .. } | StmtKind::Default));
        let end_idx = stmts
            .iter()
            .position(|s| matches!(s.kind, StmtKind::SwitchEnd));
        let prologue = start_idx.map(|si| (si + 1)..first_case.unwrap_or(stmts.len()));

        let l_end = self.asm.new_label();
        self.break_targets.push(l_end);
        self.scopes.push(HashMap::new());

        // Prologue: always runs, before the dispatch compares.
        if let Some(range) = prologue.clone() {
            for st in &stmts[range] {
                self.gen_stmt(st)?;
            }
        }

        // One label per case/default.
        let mut label_at: HashMap<usize, usize> = HashMap::new();
        let mut default_label: Option<usize> = None;
        let end_label = end_idx.map(|_| self.asm.new_label());
        for (i, st) in stmts.iter().enumerate() {
            match &st.kind {
                StmtKind::Case { .. } => {
                    label_at.insert(i, self.asm.new_label());
                }
                StmtKind::Default => {
                    let l = self.asm.new_label();
                    label_at.insert(i, l);
                    default_label = Some(l);
                }
                _ => {}
            }
        }
        // No case matched: fall to default, else the epilogue, else the exit.
        let gap_target = default_label.or(end_label).unwrap_or(l_end);

        // Compare-chain dispatch.
        for (i, st) in stmts.iter().enumerate() {
            if let StmtKind::Case { lo, hi } = &st.kind {
                let target = label_at[&i];
                match hi {
                    None => {
                        self.gen_expr(lo)?; // rax = case value
                        self.asm.mov_rcx_rax(); // rcx = case value
                        self.asm.load_local(voff, 8, false); // rax = v
                        self.asm.cmp_rax_rcx();
                        self.asm.je(target);
                    }
                    Some(hi) => {
                        // lo <= v <= hi
                        let skip = self.asm.new_label();
                        self.gen_expr(lo)?;
                        self.asm.mov_rcx_rax();
                        self.asm.load_local(voff, 8, false);
                        self.asm.cmp_rax_rcx();
                        self.asm.jl(skip); // v < lo
                        self.gen_expr(hi)?;
                        self.asm.mov_rcx_rax();
                        self.asm.load_local(voff, 8, false);
                        self.asm.cmp_rax_rcx();
                        self.asm.jg(skip); // v > hi
                        self.asm.jmp(target);
                        self.asm.place(skip);
                    }
                }
            }
        }
        self.asm.jmp(gap_target);

        // Emit the body, placing each case/default label and the epilogue marker.
        for (i, st) in stmts.iter().enumerate() {
            if prologue.as_ref().is_some_and(|r| r.contains(&i)) {
                continue; // already emitted as the prologue
            }
            if let Some(&l) = label_at.get(&i) {
                self.asm.place(l);
            }
            match &st.kind {
                StmtKind::Case { .. } | StmtKind::Default | StmtKind::SwitchStart => {}
                StmtKind::SwitchEnd => {
                    if let Some(l) = end_label {
                        self.asm.place(l);
                    }
                }
                _ => self.gen_stmt(st)?,
            }
        }
        self.scopes.pop();
        self.break_targets.pop();
        self.asm.place(l_end);
        Ok(())
    }

    /// Emit the initialiser store for a top-level global. Storage is BSS (zeroed),
    /// so only the provided value is written, at the declaration's program point.
    fn gen_global_init(&mut self, d: &Declarator, init: &Expr) -> Result<(), CodegenError> {
        let off = self.globals[&d.name].off;
        if is_f64(&d.ty) {
            self.gen_foperand(init)?; // xmm0 = value (converts int → double)
            self.asm.lea_rax_global(off);
            self.asm.movsd_store_at(RAX);
        } else if is_scalar(&d.ty) {
            let size = self.size_of(&d.ty);
            self.gen_int_expr(init, &d.ty)?; // rax = value
            self.asm.push_rax();
            self.asm.lea_rax_global(off); // rax = &global
            self.asm.mov_rcx_rax();
            self.asm.pop_rax();
            self.asm.store_through(size); // [rcx] = rax
        } else if matches!(d.ty, Type::Named(_)) && !is_brace_init(init) {
            let size = self.size_of(&d.ty);
            self.gen_expr(init)?; // rax = source address
            self.asm.mov_rsi_rax();
            self.asm.lea_rax_global(off);
            self.asm.mov_rdi_rax();
            self.asm.mov_rcx_imm32(size);
            self.asm.rep_movsb();
        } else {
            return Err(CodegenError::at(
                d.span.pos,
                "x86_64 backend: array/brace global initializers are unsupported yet",
            ));
        }
        Ok(())
    }

    /// Evaluate `cond`; jump to `target` if it is false (zero).
    fn gen_cond_jump_false(&mut self, cond: &Expr, target: usize) -> Result<(), CodegenError> {
        self.gen_expr(cond)?;
        self.asm.test_rax();
        self.asm.je(target);
        Ok(())
    }

    // ---- expressions (result in rax) ----

    fn gen_expr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let pos = e.span.pos;
        // An F64-typed expression evaluates in the SSE file. Reaching `gen_expr`
        // means integer context, so convert toward zero (signed; `gen_int_expr`
        // handles an unsigned target).
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)?;
            self.asm.cvttsd2si(RAX, 0);
            return Ok(());
        }
        match &e.kind {
            ExprKind::Int(v) | ExprKind::Char(v) => self.asm.mov_rax_imm(*v),
            ExprKind::Cast { expr, .. } => {
                // F64-target casts go through gen_fexpr; here the target is
                // integer/pointer. A float source needs a real conversion; an
                // integer source just evaluates (width handled at use/store sites).
                if is_f64(&self.expr_ty(expr)) {
                    self.gen_fexpr(expr)?;
                    if is_unsigned_int(&self.expr_ty(e)) {
                        self.gen_f64_to_u64();
                    } else {
                        self.asm.cvttsd2si(RAX, 0);
                    }
                } else {
                    self.gen_expr(expr)?;
                }
            }
            ExprKind::Sizeof(arg) => {
                let n = match arg {
                    SizeofArg::Type(t) => self.layouts.size_of(t),
                    SizeofArg::Expr(ex) => self.layouts.size_of(&self.expr_ty(ex)),
                };
                self.asm.mov_rax_imm(n as i64);
            }
            ExprKind::Str(s) => {
                // A string literal is a pointer to its NUL-terminated bytes.
                let mut bytes = s.clone().into_bytes();
                bytes.push(0);
                let idx = self.asm.intern(&bytes);
                self.asm.lea_rax_string(idx);
            }
            ExprKind::Ident(name) => match name.as_str() {
                "NULL" | "FALSE" => self.asm.mov_rax_imm(0),
                "TRUE" => self.asm.mov_rax_imm(1),
                _ => {
                    if let Some(v) = self.lookup(name) {
                        if v.indirect {
                            self.asm.load_local(v.off, 8, false); // array param: the data pointer
                        } else if is_aggregate(&v.ty) {
                            self.asm.lea_local(v.off); // array/class decays to its address
                        } else {
                            let (size, signed) = (self.size_of(&v.ty), type_signed(&v.ty));
                            self.asm.load_local(v.off, size, signed);
                        }
                    } else if let Some(g) = self.globals.get(name).cloned() {
                        self.asm.lea_rax_global(g.off); // rax = &global
                        if !is_aggregate(&g.ty) {
                            // a scalar global loads through its address; an aggregate
                            // decays to the address already in rax.
                            self.asm
                                .load_through(self.size_of(&g.ty), type_signed(&g.ty));
                        }
                    } else if self.funcs.contains_key(name) {
                        // A bare function name is a call: `Main;` runs `Main()`.
                        self.gen_call_by_name(name, &[], pos)?;
                    } else {
                        return Err(CodegenError::at(
                            pos,
                            format!("x86_64 backend: `{name}` is undeclared"),
                        ));
                    }
                }
            },
            ExprKind::Unary {
                op: UnOp::AddrOf,
                expr,
            } => self.gen_addr(expr)?,
            ExprKind::Unary {
                op: UnOp::Deref, ..
            }
            | ExprKind::Index { .. }
            | ExprKind::Member { .. } => self.gen_lvalue_value(e)?,
            ExprKind::Unary { op, expr } => self.gen_unary(*op, expr, pos)?,
            ExprKind::Postfix { op, expr } => {
                self.gen_incdec(expr, false, matches!(op, PostOp::Inc), pos)?
            }
            ExprKind::Binary { op, lhs, rhs } => self.gen_binary(*op, lhs, rhs)?,
            ExprKind::Assign { op, target, value } => self.gen_assign(*op, target, value, pos)?,
            ExprKind::Ternary { cond, then, else_ } => {
                let l_else = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.gen_cond_jump_false(cond, l_else)?;
                self.gen_expr(then)?;
                self.asm.jmp(l_end);
                self.asm.place(l_else);
                self.gen_expr(else_)?;
                self.asm.place(l_end);
            }
            ExprKind::Call { callee, args } => self.gen_call(callee, args, pos)?,
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: unsupported expression",
                ));
            }
        }
        Ok(())
    }

    fn gen_unary(&mut self, op: UnOp, inner: &Expr, pos: Pos) -> Result<(), CodegenError> {
        match op {
            UnOp::Pos => self.gen_expr(inner)?,
            UnOp::Neg => {
                self.gen_expr(inner)?;
                self.asm.neg_rax();
            }
            UnOp::BitNot => {
                self.gen_expr(inner)?;
                self.asm.not_rax();
            }
            UnOp::Not => {
                self.gen_expr(inner)?;
                self.asm.test_rax();
                self.asm.setcc_movzx(SETE); // rax = (rax == 0)
            }
            UnOp::PreInc => self.gen_incdec(inner, true, true, pos)?,
            UnOp::PreDec => self.gen_incdec(inner, true, false, pos)?,
            _ => {
                return Err(CodegenError::at(
                    pos,
                    format!("x86_64 backend: unsupported unary operator {op:?}"),
                ));
            }
        }
        Ok(())
    }

    /// `++`/`--` on any scalar lvalue (integer or pointer — a pointer steps by its
    /// pointee size). Addressed through `gen_addr`, so it covers locals, globals,
    /// `*p`, `a[i]`, and members. Result is the new value (pre) or the preserved
    /// old value (post).
    fn gen_incdec(
        &mut self,
        target: &Expr,
        pre: bool,
        inc: bool,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let ty = self.expr_ty(target);
        if !is_scalar(&ty) {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: ++/-- on a non-scalar is unsupported",
            ));
        }
        let step = match elem_of(&ty) {
            Some(elem) => self.stride_of(&elem),
            None => 1,
        };
        let delta = if inc { step } else { -step };
        let size = self.size_of(&ty);
        let signed = type_signed(&ty);
        self.gen_addr(target)?; // rax = &target
        self.asm.push_rax(); // save the address
        self.asm.load_through(size, signed); // rax = old value
        self.asm.add_rax_imm32(delta); // rax = new value
        self.asm.pop_rcx(); // rcx = &target
        self.asm.store_through(size); // [rcx] = new value
        if !pre {
            self.asm.add_rax_imm32(-delta); // postfix yields the old value
        }
        Ok(())
    }

    fn gen_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<(), CodegenError> {
        use BinOp::*;
        // Short-circuit logical operators.
        if matches!(op, And | Or) {
            return self.gen_logical(lhs, rhs, op == Or);
        }
        let lt = self.expr_ty(lhs);
        let rt = self.expr_ty(rhs);
        // Floating-point comparison: operands are F64 but the result is an int.
        // (F64 arithmetic is in gen_fexpr, since its result type is F64.)
        if matches!(op, Eq | Ne | Lt | Gt | Le | Ge) && (is_f64(&lt) || is_f64(&rt)) {
            self.gen_foperand(lhs)?; // xmm0 = lhs
            self.push_f();
            self.gen_foperand(rhs)?; // xmm0 = rhs
            self.asm.movsd_rr(1, 0); // xmm1 = rhs
            self.pop_f(0); // xmm0 = lhs
            self.asm.ucomisd(0, 1); // EFLAGS from lhs vs rhs (unsigned-style)
            let setcc = match op {
                Eq => SETE,
                Ne => SETNE,
                Lt => SETB,
                Gt => SETA,
                Le => SETBE,
                Ge => SETAE,
                _ => unreachable!(),
            };
            self.asm.setcc_movzx(setcc);
            return Ok(());
        }
        // Pointer arithmetic: the integer operand scales by the pointee size.
        if matches!(op, Add | Sub) {
            if let Some(elem) = elem_of(&lt) {
                let stride = self.stride_of(&elem);
                if op == Sub && elem_of(&rt).is_some() {
                    // pointer - pointer = element count
                    self.gen_expr(lhs)?;
                    self.asm.push_rax();
                    self.gen_expr(rhs)?;
                    self.asm.mov_rcx_rax();
                    self.asm.pop_rax();
                    self.asm.sub_rax_rcx(); // byte difference
                    self.asm.mov_rcx_imm32(stride);
                    self.asm.cqo();
                    self.asm.idiv_rcx();
                    return Ok(());
                }
                self.gen_ptr_offset(lhs, rhs, stride, op == Sub)?;
                return Ok(());
            }
            if op == Add {
                if let Some(elem) = elem_of(&rt) {
                    let stride = self.stride_of(&elem);
                    self.gen_ptr_offset(rhs, lhs, stride, false)?;
                    return Ok(());
                }
            }
        }
        // `/`, `%`, `>>` are signedness-directed off the LEFT operand; relational
        // `< > <= >=` by the usual conversions (unsigned if EITHER is unsigned).
        let op_signed = !is_unsigned_int(&lt);
        let rel_unsigned = is_unsigned_int(&lt) || is_unsigned_int(&rt);
        // lhs -> rax (spilled), rhs -> rcx, lhs back into rax.
        self.gen_expr(lhs)?;
        self.asm.push_rax();
        self.gen_expr(rhs)?;
        self.asm.mov_rcx_rax();
        self.asm.pop_rax(); // rax = lhs, rcx = rhs
        match op {
            // Comparisons -> 0/1 in rax (signed or unsigned condition codes).
            Eq => self.asm.cmp_set(SETE),
            Ne => self.asm.cmp_set(SETNE),
            Lt => self.asm.cmp_set(if rel_unsigned { SETB } else { SETL }),
            Gt => self.asm.cmp_set(if rel_unsigned { SETA } else { SETG }),
            Le => self.asm.cmp_set(if rel_unsigned { SETBE } else { SETLE }),
            Ge => self.asm.cmp_set(if rel_unsigned { SETAE } else { SETGE }),
            _ => self.apply_int_binop(op, op_signed)?,
        }
        Ok(())
    }

    /// `ptr +/- (int * stride)` — evaluate the pointer and the integer, scale the
    /// integer by the pointee stride, and add (or subtract) it.
    fn gen_ptr_offset(
        &mut self,
        ptr: &Expr,
        int: &Expr,
        stride: i32,
        subtract: bool,
    ) -> Result<(), CodegenError> {
        self.gen_expr(ptr)?;
        self.asm.push_rax();
        self.gen_expr(int)?;
        self.asm.imul_rax_imm32(stride);
        self.asm.mov_rcx_rax();
        self.asm.pop_rax(); // rax = pointer, rcx = int*stride
        if subtract {
            self.asm.sub_rax_rcx();
        } else {
            self.asm.add_rax_rcx();
        }
        Ok(())
    }

    /// Apply an arithmetic/bitwise/shift op with `rax = lhs`, `rcx = rhs`. `signed`
    /// (keyed off the left operand's type) directs `/`, `%`, and `>>`: signed uses
    /// `idiv`/`sar`, unsigned uses `div`/`shr` (C semantics).
    fn apply_int_binop(&mut self, op: BinOp, signed: bool) -> Result<(), CodegenError> {
        use BinOp::*;
        match op {
            Add => self.asm.add_rax_rcx(),
            Sub => self.asm.sub_rax_rcx(),
            Mul => self.asm.imul_rax_rcx(),
            Div => {
                if signed {
                    self.asm.cqo();
                    self.asm.idiv_rcx();
                } else {
                    self.asm.div_rcx();
                }
            }
            Mod => {
                if signed {
                    self.asm.cqo();
                    self.asm.idiv_rcx();
                } else {
                    self.asm.div_rcx();
                }
                self.asm.mov_rax_rdx();
            }
            BitAnd => self.asm.and_rax_rcx(),
            BitOr => self.asm.or_rax_rcx(),
            BitXor => self.asm.xor_rax_rcx(),
            Shl => self.asm.shl_rax_cl(),
            Shr => {
                if signed {
                    self.asm.sar_rax_cl();
                } else {
                    self.asm.shr_rax_cl();
                }
            }
            other => {
                return Err(CodegenError::new(
                    format!("x86_64 backend: unsupported operator {other:?}"),
                    None,
                ));
            }
        }
        Ok(())
    }

    fn gen_logical(&mut self, lhs: &Expr, rhs: &Expr, is_or: bool) -> Result<(), CodegenError> {
        // `a && b`: 1 iff both truthy; `a || b`: 1 iff either truthy.
        let l_short = self.asm.new_label(); // the short-circuit outcome
        let l_end = self.asm.new_label();
        self.gen_expr(lhs)?;
        self.asm.test_rax();
        if is_or {
            self.asm.jne(l_short); // a truthy -> result 1
        } else {
            self.asm.je(l_short); // a falsy -> result 0
        }
        self.gen_expr(rhs)?;
        self.asm.test_rax();
        self.asm.setcc_movzx(SETNE); // result = (b != 0)
        self.asm.jmp(l_end);
        self.asm.place(l_short);
        self.asm.mov_rax_imm(if is_or { 1 } else { 0 });
        self.asm.place(l_end);
        Ok(())
    }

    fn gen_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let tty = self.expr_ty(target);
        let size = self.size_of(&tty);
        if op == AssignOp::Assign && matches!(tty, Type::Named(_)) {
            // Whole-class assignment is a deep copy.
            self.gen_addr(target)?; // dest address
            self.asm.push_rax();
            self.gen_expr(value)?; // source address
            self.asm.mov_rsi_rax(); // rsi = src
            self.asm.pop_rax();
            self.asm.mov_rdi_rax(); // rdi = dest
            self.asm.mov_rcx_imm32(size);
            self.asm.rep_movsb();
            return Ok(());
        }
        if op == AssignOp::Assign {
            // &target -> stack, value -> rax, store value through the address.
            self.gen_addr(target)?;
            self.asm.push_rax();
            self.gen_int_expr(value, &tty)?; // float source converts per signedness
            self.asm.pop_rcx();
            self.asm.store_through(size); // [rcx] = rax (low `size` bytes)
            return Ok(()); // assignment's value is in rax
        }
        // Compound `x op= value` on an integer scalar lvalue.
        if !is_scalar(&tty) || elem_of(&tty).is_some() {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: compound assignment to a pointer/aggregate is unsupported yet",
            ));
        }
        self.gen_addr(target)?; // rax = &x
        self.asm.push_rax();
        self.asm.load_through(size, type_signed(&tty)); // rax = current x
        self.asm.push_rax();
        self.gen_expr(value)?; // rax = value
        self.asm.mov_rcx_rax(); // rcx = value
        self.asm.pop_rax(); // rax = current x
        // `>>=`/`/=`/`%=` are directed by the lvalue's signedness.
        self.apply_int_binop(compound_binop(op), !is_unsigned_int(&tty))?; // rax = x op value
        self.asm.pop_rcx(); // rcx = &x
        self.asm.store_through(size);
        Ok(())
    }

    // ---- lvalues / addresses (result in rax) ----

    /// Compute the address of an lvalue into rax.
    fn gen_addr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Ident(name) => self.gen_addr_ident(name, pos)?,
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => self.gen_expr(expr)?, // the pointer value *is* the address
            ExprKind::Index { base, index } => {
                let bty = self.expr_ty(base);
                let elem = elem_of(&bty).ok_or_else(|| {
                    CodegenError::at(pos, "x86_64 backend: cannot index a non-array/pointer")
                })?;
                let stride = self.stride_of(&elem);
                // An array decays to its address; a pointer's value is the address.
                if is_array(&bty) {
                    self.gen_addr(base)?;
                } else {
                    self.gen_expr(base)?;
                }
                self.asm.push_rax();
                self.gen_expr(index)?;
                self.asm.imul_rax_imm32(stride); // index * stride
                self.asm.pop_rcx(); // base address
                self.asm.add_rax_rcx(); // base + index*stride
            }
            ExprKind::Member { base, field, arrow } => {
                // Resolve the class and load its base address into rax.
                let class = if *arrow {
                    self.gen_expr(base)?; // pointer value is the class address
                    named_of(&pointee(&self.expr_ty(base)), pos)?
                } else if is_place(base) {
                    self.gen_addr(base)?;
                    named_of(&self.expr_ty(base), pos)?
                } else if is_aggregate(&self.expr_ty(base)) {
                    // A class rvalue (e.g. a class-returning call `Mk().x`) is
                    // carried by address, which `gen_expr` leaves in rax.
                    self.gen_expr(base)?;
                    named_of(&self.expr_ty(base), pos)?
                } else {
                    return Err(CodegenError::at(
                        pos,
                        "x86_64 backend: member access on a non-lvalue is unsupported yet",
                    ));
                };
                let off = self.layouts.offset_of(&class, field).ok_or_else(|| {
                    CodegenError::at(
                        pos,
                        format!("x86_64 backend: no field `{field}` on `{class}`"),
                    )
                })? as i32;
                if off != 0 {
                    self.asm.add_rax_imm32(off);
                }
            }
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: expression is not an lvalue",
                ));
            }
        }
        Ok(())
    }

    fn gen_addr_ident(&mut self, name: &str, pos: Pos) -> Result<(), CodegenError> {
        if let Some(v) = self.lookup(name) {
            if v.indirect {
                self.asm.load_local(v.off, 8, false); // the slot holds the data pointer
            } else {
                self.asm.lea_local(v.off); // the slot/array data starts here
            }
            return Ok(());
        }
        if let Some(g) = self.globals.get(name) {
            self.asm.lea_rax_global(g.off);
            return Ok(());
        }
        Err(CodegenError::at(
            pos,
            format!("x86_64 backend: `{name}` is undeclared"),
        ))
    }

    /// Load the value of an lvalue into rax. An aggregate (array/class) decays to
    /// its address; any other lvalue is loaded width-aware through its address.
    fn gen_lvalue_value(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let ty = self.expr_ty(e);
        self.gen_addr(e)?;
        if !is_aggregate(&ty) {
            self.asm.load_through(self.size_of(&ty), type_signed(&ty));
        }
        Ok(())
    }

    /// Zero-fill `size` bytes of the local at `off` (8/4/2/1-byte stores).
    fn gen_zero(&mut self, off: i32, size: i32) {
        self.asm.mov_rax_imm(0);
        let mut o = 0;
        for chunk in [8, 4, 2, 1] {
            while size - o >= chunk {
                self.asm.store_local(off - o, chunk);
                o += chunk;
            }
        }
    }

    /// Copy `size` bytes from the address in rax (source) to `dest` (a local
    /// offset) — a by-value aggregate copy (`rep movsb`).
    fn gen_memcpy_to_local(&mut self, dest_off: i32, size: i32) {
        self.asm.mov_rsi_rax(); // rsi = source
        self.asm.lea_local(dest_off);
        self.asm.mov_rdi_rax(); // rdi = dest
        self.asm.mov_rcx_imm32(size);
        self.asm.rep_movsb();
    }

    fn gen_call(&mut self, callee: &Expr, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        match &callee.kind {
            ExprKind::Ident(n) => self.gen_call_by_name(n, args, pos),
            _ => Err(CodegenError::at(
                pos,
                "x86_64 backend: only direct function calls are supported yet",
            )),
        }
    }

    fn gen_call_by_name(
        &mut self,
        name: &str,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        // A user function shadows nothing; otherwise a builtin is lowered inline
        // (the x86-64 backend has no libc, so each is emitted from scratch).
        let label = match self.funcs.get(name) {
            Some(&l) => l,
            None if crate::builtins::is_builtin(name) => return self.gen_builtin(name, args, pos),
            None => {
                return Err(CodegenError::at(
                    pos,
                    format!("x86_64 backend: call to unknown function `{name}`"),
                ));
            }
        };
        let (param_tys, ret_ty) = self
            .funcs_sig
            .get(name)
            .cloned()
            .unwrap_or_else(|| (Vec::new(), Type::I64));
        // A class-returning function uses an sret pointer: the caller allocates a
        // result temp in its frame and hands its address to the callee in r11.
        let sret_slot = if matches!(ret_ty, Type::Named(_)) {
            Some(self.alloc(self.size_of(&ret_ty).max(1), self.align_of(&ret_ty)))
        } else {
            None
        };
        // For a variadic callee, only the named params are register-passed; the
        // trailing args go into a frame buffer whose address + count are passed as
        // two hidden integer args (see below).
        let varargs = self.funcs_va.get(name).copied().unwrap_or(false);
        let n_named = if varargs {
            param_tys.len().min(args.len())
        } else {
            args.len()
        };
        let named = &args[..n_named];
        let extra = &args[n_named..];
        // Class each named arg: F64 → an xmm register (xmm0..), everything else →
        // a GP register (rdi..). Reject overflow of either class.
        let mut gpr = 0usize;
        let mut fpr = 0usize;
        let mut targets = Vec::with_capacity(named.len());
        for (i, _) in named.iter().enumerate() {
            let pty = param_tys.get(i).cloned().unwrap_or(Type::I64);
            if is_f64(&pty) {
                if fpr >= 8 {
                    return Err(CodegenError::at(
                        pos,
                        "x86_64 backend: at most 8 F64 arguments",
                    ));
                }
                targets.push((true, fpr, pty));
                fpr += 1;
            } else {
                if gpr >= ARG_REGS {
                    return Err(CodegenError::at(
                        pos,
                        "x86_64 backend: at most 6 integer arguments",
                    ));
                }
                targets.push((false, gpr, pty));
                gpr += 1;
            }
        }
        // Evaluate named args left to right, each spilled to the machine stack (a
        // float is moved through a GPR first).
        for (arg, (is_float, _, pty)) in named.iter().zip(&targets) {
            if *is_float {
                self.gen_foperand(arg)?; // xmm0 = value
                self.asm.movq_r_from_xmm(RAX, 0);
                self.asm.push_rax();
            } else {
                self.gen_int_expr(arg, pty)?; // rax = value
                self.asm.push_rax();
            }
        }
        // Variadic: stage the trailing args into a frame buffer (8 bytes each, an
        // F64 by its bit pattern), then push va_ptr + va_cnt for the next two int
        // registers. Buffer element j is at slot offset `off - j*8`.
        let va = if varargs && !extra.is_empty() {
            if gpr + 1 >= ARG_REGS {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: too many integer args before `...`",
                ));
            }
            let k = extra.len() as i32;
            let off = self.alloc(k * 8, 8);
            for (j, arg) in extra.iter().enumerate() {
                if is_f64(&self.expr_ty(arg)) {
                    self.gen_foperand(arg)?;
                    self.asm.movq_r_from_xmm(RAX, 0);
                } else {
                    self.gen_expr(arg)?;
                }
                self.asm.store_local(off - j as i32 * 8, 8);
            }
            self.asm.lea_local(off); // va_ptr = &buffer
            self.asm.push_rax();
            self.asm.mov_ri(RAX, k);
            self.asm.push_rax(); // va_cnt
            Some(gpr)
        } else {
            None
        };
        // Pop the hidden args first (they were pushed last): va_cnt then va_ptr.
        if let Some(gpr_named) = va {
            self.asm.pop_argreg(gpr_named + 1); // va_cnt
            self.asm.pop_argreg(gpr_named); // va_ptr
        }
        for (is_float, idx, _) in targets.iter().rev() {
            if *is_float {
                self.asm.pop_rax();
                self.asm.movq_xmm_from_r(*idx as u8, RAX);
            } else {
                self.asm.pop_argreg(*idx);
            }
        }
        // Hand the result temp's address to the callee in r11 (set last, after the
        // arg registers, so nothing clobbers it before the call).
        if let Some(off) = sret_slot {
            self.asm.lea_local(off); // rax = &temp
            self.asm.mov_rr(R11, RAX);
        }
        self.asm.call(label);
        if let Some(off) = sret_slot {
            self.asm.lea_local(off); // the class rvalue *is* the temp's address
        }
        Ok(())
    }

    // ---- builtins (no libc: each is lowered inline or to an emitted routine) ----

    /// Evaluate `args` (as integers/pointers) into the System V argument registers
    /// rdi, rsi, rdx, rcx, …, ready for a builtin runtime routine.
    fn gen_int_args(&mut self, args: &[&Expr]) -> Result<(), CodegenError> {
        for a in args {
            self.gen_int_expr(a, &Type::I64)?;
            self.asm.push_rax();
        }
        for i in (0..args.len()).rev() {
            self.asm.pop_argreg(i);
        }
        Ok(())
    }

    /// Evaluate `args` into the argument registers, then call runtime routine
    /// `routine` (result in rax).
    fn call_rt(&mut self, routine: &'static str, args: &[&Expr]) -> Result<(), CodegenError> {
        self.gen_int_args(args)?;
        let l = self.rt_routine(routine);
        self.asm.call(l);
        Ok(())
    }

    /// Lower a builtin call. Integer/pointer/string/memory builtins go to emitted
    /// runtime routines; the SSE-feasible `F64` ones are inlined. The rest
    /// (transcendental math, the sprintf family) are not supported without libc.
    fn gen_builtin(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        // F64 builtins doable with SSE2 (result in xmm0).
        match name {
            "Sqrt" => {
                self.gen_foperand(&args[0])?;
                self.asm.sqrtsd(0, 0);
                return Ok(());
            }
            "Fabs" => {
                self.gen_foperand(&args[0])?;
                self.asm.mov_ri64(RAX, 0x7FFF_FFFF_FFFF_FFFF); // clear the sign bit
                self.asm.movq_xmm_from_r(1, RAX);
                self.asm.andpd(0, 1);
                return Ok(());
            }
            "Floor" => {
                self.gen_foperand(&args[0])?;
                self.asm.roundsd(0, 0, 1); // toward −∞
                return Ok(());
            }
            "Ceil" => {
                self.gen_foperand(&args[0])?;
                self.asm.roundsd(0, 0, 2); // toward +∞
                return Ok(());
            }
            "Round" => {
                // Round half *away from zero* (matching `f64::round` / arm64
                // `frinta`), which `roundsd`'s nearest-*even* mode does not do.
                // Take t = trunc(x), the exact fractional part d = x − t, and bump
                // t by copysign(1, x) when |d| ≥ 0.5. Scratch: xmm1..xmm5.
                self.gen_foperand(&args[0])?; // xmm0 = x
                self.asm.roundsd(1, 0, 3); // xmm1 = t = trunc(x)
                self.asm.movsd_rr(2, 0); // xmm2 = x
                self.asm.subsd(2, 1); // xmm2 = d = x − t
                self.asm.mov_ri64(RAX, 0x7FFF_FFFF_FFFF_FFFF);
                self.asm.movq_xmm_from_r(3, RAX);
                self.asm.andpd(2, 3); // xmm2 = |d|
                self.asm.mov_ri64(RAX, 0x3FE0_0000_0000_0000); // 0.5
                self.asm.movq_xmm_from_r(3, RAX);
                let skip = self.asm.new_label();
                self.asm.ucomisd(2, 3); // |d| vs 0.5
                self.asm.jb(skip); // |d| < 0.5 → no bump
                // bump = copysign(1.0, x) = 1.0 | (x & signbit)
                self.asm.mov_ri64(RAX, 0x3FF0_0000_0000_0000); // 1.0
                self.asm.movq_xmm_from_r(4, RAX);
                self.asm.mov_ri64(RAX, 0x8000_0000_0000_0000u64); // sign mask
                self.asm.movq_xmm_from_r(5, RAX);
                self.asm.andpd(5, 0); // xmm5 = x & signbit
                self.asm.orpd(4, 5); // xmm4 = copysign(1.0, x)
                self.asm.addsd(1, 4); // t += bump
                self.asm.place(skip);
                self.asm.movsd_rr(0, 1); // result in xmm0
                return Ok(());
            }
            _ => {}
        }
        match name {
            // `Free` is a no-op (the bump allocator never reclaims); still evaluate
            // the argument for its side effects.
            "Free" => {
                self.gen_expr(&args[0])?;
                Ok(())
            }
            "RandU64" => {
                let l = self.rt_routine("RandU64");
                self.asm.call(l);
                Ok(())
            }
            // Clock/time primitives via the Linux syscalls (clock_gettime nr 228,
            // nanosleep nr 35) over a BSS timespec. CLOCK_REALTIME=0, MONOTONIC=1.
            "UnixNS" | "NanoNS" => {
                let scratch = self.alloc_bss(16, 8);
                if name == "NanoNS" {
                    self.os.emit_mono_ns(&mut self.asm, scratch);
                } else {
                    self.os.emit_unix_ns(&mut self.asm, scratch);
                }
                Ok(())
            }
            "Sleep" => {
                self.gen_expr(&args[0])?; // rax = ns
                let scratch = self.alloc_bss(16, 8);
                self.os.emit_sleep(&mut self.asm, scratch);
                Ok(())
            }
            // Variadic-argument access: read the hidden va buffer the prologue saved.
            "VarArgCnt" => {
                let (_, cnt_off) = self.cur_va.ok_or_else(|| {
                    CodegenError::at(pos, "x86_64 backend: VarArg* outside a variadic function")
                })?;
                self.asm.load_local(cnt_off, 8, false); // rax = va_cnt
                Ok(())
            }
            "VarArgI64" | "VarArg" | "VarArgF64" => {
                let (ptr_off, _) = self.cur_va.ok_or_else(|| {
                    CodegenError::at(pos, "x86_64 backend: VarArg* outside a variadic function")
                })?;
                self.gen_expr(&args[0])?; // rax = i
                self.asm.imul_rax_imm32(8); // rax = i*8
                self.asm.mov_rr(RCX, RAX); // rcx = i*8
                self.asm.load_local(ptr_off, 8, false); // rax = va_ptr
                self.asm.add_rr(RAX, RCX); // rax = va_ptr + i*8
                self.asm.load_qword_at(RAX, RAX); // rax = slot bits
                if name == "VarArgF64" {
                    self.asm.movq_xmm_from_r(0, RAX); // reinterpret as a double in xmm0
                }
                Ok(())
            }
            // `ArgC()` / `ArgV(i)` — read the command line captured at the entry
            // into the argc/argv BSS slots.
            "ArgC" => {
                let off = self
                    .argc_off
                    .expect("argc slot reserved when args are used");
                self.asm.lea_global(RAX, off);
                self.asm.load_qword_at(RAX, RAX); // rax = argc
                Ok(())
            }
            "ArgV" => {
                let (cv, av) = (self.argc_off.unwrap(), self.argv_off.unwrap());
                self.gen_expr(&args[0])?; // rax = i
                self.asm.mov_rr(RCX, RAX); // rcx = i
                self.asm.lea_global(RDX, cv);
                self.asm.load_qword_at(RDX, RDX); // rdx = argc
                let null = self.asm.new_label();
                let done = self.asm.new_label();
                self.asm.cmp_reg_reg(RCX, RDX);
                self.asm.jae(null); // unsigned i >= argc (also catches i < 0)
                self.asm.lea_global(RAX, av);
                self.asm.load_qword_at(RAX, RAX); // rax = argv base pointer
                self.asm.load_qword_idx8(RAX, RAX, RCX); // rax = argv[i]
                self.asm.jmp(done);
                self.asm.place(null);
                self.asm.mov_ri(RAX, 0); // NULL
                self.asm.place(done);
                Ok(())
            }
            // printf-into-a-buffer (the sprintf family)
            "StrPrint" => self.gen_strprint(args, false, pos),
            "CatPrint" => self.gen_strprint(args, true, pos),
            "MStrPrint" => self.gen_mstrprint(args, pos),
            "I64ToStr" => self.gen_i64tostr(args, pos),
            "F64ToStr" => self.gen_f64tostr(args, pos),
            // one integer/pointer argument
            "MAlloc" => self.call_rt("MAlloc", &[&args[0]]),
            "StrLen" => self.call_rt("StrLen", &[&args[0]]),
            "StrToUpper" => self.call_rt("StrToUpper", &[&args[0]]),
            "StrToLower" => self.call_rt("StrToLower", &[&args[0]]),
            "StrRev" => self.call_rt("StrRev", &[&args[0]]),
            "Abs" => self.call_rt("Abs", &[&args[0]]),
            "Sign" => self.call_rt("Sign", &[&args[0]]),
            "ToUpper" => self.call_rt("ToUpper", &[&args[0]]),
            "ToLower" => self.call_rt("ToLower", &[&args[0]]),
            // two arguments
            "StrCmp" => self.call_rt("StrCmp", &[&args[0], &args[1]]),
            "StrCpy" => self.call_rt("StrCpy", &[&args[0], &args[1]]),
            "StrCat" => self.call_rt("StrCat", &[&args[0], &args[1]]),
            "StrChr" => self.call_rt("StrChr", &[&args[0], &args[1]]),
            "StrLastChr" => self.call_rt("StrLastChr", &[&args[0], &args[1]]),
            "StrFind" => self.call_rt("StrFind", &[&args[0], &args[1]]),
            "StrSpn" => self.call_rt("StrSpn", &[&args[0], &args[1]]),
            "StrCSpn" => self.call_rt("StrCSpn", &[&args[0], &args[1]]),
            // three arguments
            "StrNCmp" => self.call_rt("StrNCmp", &[&args[0], &args[1], &args[2]]),
            "StrNCpy" => self.call_rt("StrNCpy", &[&args[0], &args[1], &args[2]]),
            "MemCpy" => self.call_rt("MemCpy", &[&args[0], &args[1], &args[2]]),
            "MemMove" => self.call_rt("MemMove", &[&args[0], &args[1], &args[2]]),
            "MemSet" => self.call_rt("MemSet", &[&args[0], &args[1], &args[2]]),
            "MemCmp" => self.call_rt("MemCmp", &[&args[0], &args[1], &args[2]]),
            "MemFind" => self.call_rt("MemFind", &[&args[0], &args[1], &args[2]]),
            // four arguments
            "MemSearch" => self.call_rt("MemSearch", &[&args[0], &args[1], &args[2], &args[3]]),
            // the `Is*` ctype predicates — each emitted as an inline range-check
            // routine (see `emit_rt_ctype`).
            n if crate::builtins::ctype_ranges(n).is_some() => {
                let name = *crate::builtins::CTYPE_NAMES
                    .iter()
                    .find(|&&s| s == n)
                    .unwrap();
                self.call_rt(name, &[&args[0]])
            }
            other => Err(CodegenError::at(
                pos,
                format!("x86_64 backend: builtin `{other}` is not supported yet"),
            )),
        }
    }

    /// `StrPrint(dst, fmt, …)` / `CatPrint(dst, fmt, …)`: printf-format into `dst`
    /// (or `dst + StrLen(dst)` for `append`), NUL-terminate, return `dst`. Reuses
    /// the print machinery by pointing the output sink (`out_ptr`) at the buffer.
    fn gen_strprint(&mut self, args: &[Expr], append: bool, pos: Pos) -> Result<(), CodegenError> {
        let ExprKind::Str(fmt) = &args[1].kind else {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: StrPrint/CatPrint format must be a string literal",
            ));
        };
        let fmt = fmt.clone();
        let out = self.out_ptr_global();
        // Evaluate dst once; keep it (the return value) in a frame slot.
        self.gen_expr(&args[0])?;
        let dslot = self.alloc(8, 8);
        self.asm.store_local(dslot, 8);
        // Point out_ptr at the write start: dst, or dst + StrLen(dst) for CatPrint.
        if append {
            self.asm.load_local(dslot, 8, false);
            self.asm.mov_rdi_rax();
            let sl = self.rt_routine("StrLen");
            self.asm.call(sl); // rax = StrLen(dst)
            self.asm.mov_rcx_rax();
            self.asm.load_local(dslot, 8, false);
            self.asm.add_rax_rcx(); // rax = dst + len
        } else {
            self.asm.load_local(dslot, 8, false);
        }
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = start
        self.gen_print(&fmt, &args[2..], pos)?; // appends to the buffer
        self.finish_buffer_write(out);
        self.asm.load_local(dslot, 8, false); // return dst
        Ok(())
    }

    /// `I64ToStr(n, buf)`: format `n` as a decimal string into `buf`; return `buf`.
    fn gen_i64tostr(&mut self, args: &[Expr], _pos: Pos) -> Result<(), CodegenError> {
        let out = self.out_ptr_global();
        self.gen_expr(&args[1])?; // buf
        let dslot = self.alloc(8, 8);
        self.asm.store_local(dslot, 8);
        self.asm.load_local(dslot, 8, false);
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = buf
        self.gen_int_expr(&args[0], &Type::I64)?; // n
        self.asm.mov_rdi_rax();
        self.asm.mov_ri(RSI, 10); // radix
        self.asm.mov_ri(RDX, F_SIGNED); // signed decimal, no flags
        self.asm.mov_ri(RCX, 0); // width
        self.asm.mov_ri(R8, -1); // precision
        let l = self.helper(Helper::FmtInt);
        self.asm.call(l);
        self.finish_buffer_write(out);
        self.asm.load_local(dslot, 8, false); // return buf
        Ok(())
    }

    /// `F64ToStr(f, buf)`: format `f` with `%g` into `buf`; return `buf`.
    fn gen_f64tostr(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let out = self.out_ptr_global();
        self.gen_expr(&args[1])?; // buf
        let dslot = self.alloc(8, 8);
        self.asm.store_local(dslot, 8);
        self.asm.load_local(dslot, 8, false);
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = buf
        self.gen_print("%g", &args[0..1], pos)?; // format the value (matches the interpreter)
        self.finish_buffer_write(out);
        self.asm.load_local(dslot, 8, false); // return buf
        Ok(())
    }

    /// `MStrPrint(fmt, ...)`: format into a fresh, right-sized buffer and return it
    /// (asprintf-style). This is the **growing-sink** strategy real libcs use for
    /// `vasprintf` (glibc/BSD's `open_memstream`): allocate a small owned buffer,
    /// arm the sink to *grow* it (realloc + copy via [`Helper::GrowSink`]) whenever
    /// a write would overflow, then format in a single pass. No measurement pass
    /// and no fixed cap — the result pointer is just `out_base` when we finish.
    fn gen_mstrprint(&mut self, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let ExprKind::Str(fmt) = &args[0].kind else {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: MStrPrint's format must be a string literal",
            ));
        };
        let fmt = fmt.clone();
        const INIT_CAP: i32 = 64; // grown on demand; covers most results in one shot
        let out = self.out_ptr_global();
        let base = self.out_base_global();
        let limit = self.out_limit_global();
        self.helper(Helper::GrowSink); // ensure the grow routine is emitted
        // buf = MAlloc(INIT_CAP); arm the sink at it (base = ptr = buf, limit = end).
        self.asm.mov_ri(RDI, INIT_CAP);
        let malloc = self.rt_routine("MAlloc");
        self.asm.call(malloc); // rax = buf
        self.asm.lea_global(R8, base);
        self.asm.store_qword_at(R8, RAX); // out_base = buf
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = buf (write cursor)
        self.asm.add_ri(RAX, INIT_CAP); // rax = buf + INIT_CAP
        self.asm.lea_global(R8, limit);
        self.asm.store_qword_at(R8, RAX); // out_limit = buf + INIT_CAP
        // Format in one pass; the sink grows the buffer as needed.
        self.gen_print(&fmt, &args[1..], pos)?;
        // NUL-terminate at the cursor (GrowSink keeps cursor < limit, so this fits),
        // then disarm: cursor = 0 (stdout), limit = 0 (StrPrint append mode).
        self.asm.lea_global(R8, out);
        self.asm.load_qword_at(RAX, R8); // rax = cursor
        self.asm.store_byte_imm_at(RAX, 0); // *cursor = '\0'
        self.asm.mov_ri(RAX, 0);
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = 0
        self.asm.lea_global(R8, limit);
        self.asm.store_qword_at(R8, RAX); // out_limit = 0
        // Return the buffer base (it may have moved during a grow).
        self.asm.lea_global(R8, base);
        self.asm.load_qword_at(RAX, R8); // rax = out_base = result
        Ok(())
    }

    /// NUL-terminate the buffer at `out_ptr` and reset the sink back to stdout.
    fn finish_buffer_write(&mut self, out: i32) {
        self.asm.lea_global(R8, out);
        self.asm.load_qword_at(RAX, R8); // rax = end of written data
        self.asm.store_byte_imm_at(RAX, 0); // NUL terminator
        self.asm.mov_ri(RAX, 0);
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RAX); // out_ptr = 0 (stdout again)
    }

    /// Emit the bodies of the builtin runtime routines actually used (in a fixed
    /// order for reproducibility). Each follows the internal ABI: arguments in
    /// rdi/rsi/rdx/rcx, result in rax, clobbering only volatile registers.
    fn emit_rt_routines(&mut self) {
        const ORDER: &[&str] = &[
            "MAlloc",
            "RandU64",
            "StrLen",
            "StrCmp",
            "StrNCmp",
            "StrCpy",
            "StrNCpy",
            "StrCat",
            "StrChr",
            "StrLastChr",
            "StrFind",
            "StrSpn",
            "StrCSpn",
            "StrToUpper",
            "StrToLower",
            "StrRev",
            "MemCpy",
            "MemMove",
            "MemSet",
            "MemCmp",
            "MemFind",
            "MemSearch",
            "ToUpper",
            "ToLower",
            "Abs",
            "Sign",
        ];
        for &name in ORDER {
            let Some(&label) = self.rt_routines.get(name) else {
                continue;
            };
            self.asm.place(label);
            match name {
                "MAlloc" => self.emit_rt_malloc(),
                "RandU64" => self.emit_rt_randu64(),
                "StrLen" => self.emit_rt_strlen(),
                "StrCmp" => self.emit_rt_strcmp(),
                "StrNCmp" => self.emit_rt_strncmp(),
                "StrCpy" => self.emit_rt_strcpy(),
                "StrNCpy" => self.emit_rt_strncpy(),
                "StrCat" => self.emit_rt_strcat(),
                "StrChr" => self.emit_rt_strchr(),
                "StrLastChr" => self.emit_rt_strlastchr(),
                "StrFind" => self.emit_rt_strfind(),
                "StrSpn" => self.emit_rt_strspn(),
                "StrCSpn" => self.emit_rt_strcspn(),
                "StrToUpper" => self.emit_rt_strcase(true),
                "StrToLower" => self.emit_rt_strcase(false),
                "StrRev" => self.emit_rt_strrev(),
                "MemCpy" => self.emit_rt_memcpy(),
                "MemMove" => self.emit_rt_memmove(),
                "MemSet" => self.emit_rt_memset(),
                "MemCmp" => self.emit_rt_memcmp(),
                "MemFind" => self.emit_rt_memfind(),
                "MemSearch" => self.emit_rt_memsearch(),
                "ToUpper" => self.emit_rt_tocase(true),
                "ToLower" => self.emit_rt_tocase(false),
                "Abs" => self.emit_rt_abs(),
                "Sign" => self.emit_rt_sign(),
                _ => {}
            }
        }
        // The `Is*` ctype predicates (registered via `call_rt`), each an inline
        // range-check routine driven by the shared `ctype_ranges` table.
        for &name in crate::builtins::CTYPE_NAMES {
            if let Some(&label) = self.rt_routines.get(name) {
                self.asm.place(label);
                self.emit_rt_ctype(crate::builtins::ctype_ranges(name).unwrap());
            }
        }
    }

    /// An `Is*` ctype predicate routine: `rdi` = the character, result `0`/`1` in
    /// `rax`. Returns 1 on the first range `[lo, hi]` that contains the byte, else
    /// 0 — matching `builtins::ctype_ranges` and the arm64 backend.
    fn emit_rt_ctype(&mut self, ranges: &[(u8, u8)]) {
        for &(lo, hi) in ranges {
            let skip = self.asm.new_label();
            self.asm.cmp_ri(RDI, lo as i32);
            self.asm.jl(skip); // c < lo
            self.asm.cmp_ri(RDI, hi as i32);
            self.asm.jg(skip); // c > hi
            self.asm.mov_ri(RAX, 1);
            self.asm.emit(&[0xC3]); // ret (hit)
            self.asm.place(skip);
        }
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]); // ret (no range matched)
    }

    /// `MAlloc(rdi=n)`: a bump allocator over `mmap`'d chunks (1 MiB, page-aligned),
    /// 16-byte-aligned allocations. `Free` is a no-op, so chunks are never reused.
    fn emit_rt_malloc(&mut self) {
        let hp = self.alloc_bss(8, 8); // heap bump pointer
        let he = self.alloc_bss(8, 8); // heap end
        let alloc = self.asm.new_label();
        let sized = self.asm.new_label();
        self.asm.emit(&[0x53]); // push rbx (preserve; rbx survives the syscall)
        self.asm.add_ri(RDI, 15);
        self.asm.and_ri(RDI, -16);
        self.asm.mov_rr(RBX, RDI); // rbx = aligned n
        self.asm.lea_global(R9, hp);
        self.asm.load_qword_at(RAX, R9); // rax = *heap_ptr
        self.asm.lea_global(R10, he);
        self.asm.load_qword_at(R8, R10); // r8 = *heap_end
        self.asm.mov_rr(RCX, RAX);
        self.asm.add_rr(RCX, RBX); // rcx = ptr + n
        self.asm.cmp_reg_reg(RCX, R8);
        self.asm.jbe(alloc); // fits in the current chunk
        // chunk size = max(n, 1 MiB) rounded up to a page
        self.asm.mov_rr(RSI, RBX);
        self.asm.mov_ri(RCX, 0x10_0000);
        self.asm.cmp_reg_reg(RSI, RCX);
        self.asm.jae(sized);
        self.asm.mov_rr(RSI, RCX);
        self.asm.place(sized);
        self.asm.add_ri(RSI, 4095);
        self.asm.and_ri(RSI, -4096);
        // Grab a fresh page-aligned chunk of `rsi` bytes; base → rax, rsi kept.
        self.os.emit_page_alloc(&mut self.asm);
        self.asm.mov_rr(R8, RAX);
        self.asm.add_rr(R8, RSI); // r8 = base + chunk size
        self.asm.lea_global(R10, he);
        self.asm.store_qword_at(R10, R8); // *heap_end = base + size
        self.asm.place(alloc);
        // result = rax (base); *heap_ptr = rax + n
        self.asm.mov_rr(RCX, RAX);
        self.asm.add_rr(RCX, RBX);
        self.asm.lea_global(R9, hp);
        self.asm.store_qword_at(R9, RCX);
        self.asm.emit(&[0x5B]); // pop rbx
        self.asm.emit(&[0xC3]); // ret
    }

    /// `RandU64()`: a deterministic splitmix64 over a hidden zero-seeded global,
    /// matching `builtins::splitmix64` (and the interpreter) bit for bit.
    fn emit_rt_randu64(&mut self) {
        let st = self.alloc_bss(8, 8); // PRNG state (seed 0)
        self.asm.lea_global(R8, st);
        self.asm.load_qword_at(RAX, R8);
        self.asm.mov_ri64(RCX, 0x9e37_79b9_7f4a_7c15);
        self.asm.add_rr(RAX, RCX);
        self.asm.store_qword_at(R8, RAX); // state += GOLDEN
        self.asm.mov_rr(RCX, RAX);
        self.asm.shr_ri(RCX, 30);
        self.asm.xor_rr(RAX, RCX); // z ^= z >> 30
        self.asm.mov_ri64(RCX, 0xbf58_476d_1ce4_e5b9);
        self.asm.imul_rr(RAX, RCX);
        self.asm.mov_rr(RCX, RAX);
        self.asm.shr_ri(RCX, 27);
        self.asm.xor_rr(RAX, RCX);
        self.asm.mov_ri64(RCX, 0x94d0_49bb_1331_11eb);
        self.asm.imul_rr(RAX, RCX);
        self.asm.mov_rr(RCX, RAX);
        self.asm.shr_ri(RCX, 31);
        self.asm.xor_rr(RAX, RCX);
        self.asm.emit(&[0xC3]); // ret
    }

    fn emit_rt_strlen(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI);
        self.asm.place(l);
        self.asm.cmp_byte_imm_at(RAX, 0);
        self.asm.je(d);
        self.asm.inc_r(RAX);
        self.asm.jmp(l);
        self.asm.place(d);
        self.asm.sub_rr(RAX, RDI);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strcmp(&mut self) {
        let l = self.asm.new_label();
        let diff = self.asm.new_label();
        let done = self.asm.new_label();
        let eq = self.asm.new_label();
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.load_byte_zx(RDX, RSI);
        self.asm.cmp_reg_reg(RCX, RDX);
        self.asm.jne(diff);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(eq);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RSI);
        self.asm.jmp(l);
        self.asm.place(diff);
        self.asm.mov_ri(RAX, 1);
        self.asm.cmp_reg_reg(RCX, RDX);
        self.asm.jg(done);
        self.asm.mov_ri(RAX, -1);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
        self.asm.place(eq);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strncmp(&mut self) {
        let l = self.asm.new_label();
        let diff = self.asm.new_label();
        let done = self.asm.new_label();
        let eq = self.asm.new_label();
        self.asm.place(l);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(eq);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.load_byte_zx(R8, RSI);
        self.asm.cmp_reg_reg(RCX, R8);
        self.asm.jne(diff);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(eq);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RSI);
        self.asm.dec_r(RDX);
        self.asm.jmp(l);
        self.asm.place(diff);
        self.asm.mov_ri(RAX, 1);
        self.asm.cmp_reg_reg(RCX, R8);
        self.asm.jg(done);
        self.asm.mov_ri(RAX, -1);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
        self.asm.place(eq);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strcpy(&mut self) {
        let l = self.asm.new_label();
        let d = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, RSI);
        self.asm.store_byte_at(RDI, RCX);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(d);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RSI);
        self.asm.jmp(l);
        self.asm.place(d);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strncpy(&mut self) {
        let l = self.asm.new_label();
        let pad = self.asm.new_label();
        let p = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.place(l);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(done);
        self.asm.load_byte_zx(RCX, RSI);
        self.asm.store_byte_at(RDI, RCX);
        self.asm.inc_r(RDI);
        self.asm.dec_r(RDX);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(pad);
        self.asm.inc_r(RSI);
        self.asm.jmp(l);
        self.asm.place(pad);
        self.asm.place(p);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(done);
        self.asm.store_byte_imm_at(RDI, 0);
        self.asm.inc_r(RDI);
        self.asm.dec_r(RDX);
        self.asm.jmp(p);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strcat(&mut self) {
        let f = self.asm.new_label();
        let cp = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.place(f);
        self.asm.cmp_byte_imm_at(RDI, 0);
        self.asm.je(cp);
        self.asm.inc_r(RDI);
        self.asm.jmp(f);
        self.asm.place(cp);
        self.asm.load_byte_zx(RCX, RSI);
        self.asm.store_byte_at(RDI, RCX);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(done);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RSI);
        self.asm.jmp(cp);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strchr(&mut self) {
        let l = self.asm.new_label();
        let nf = self.asm.new_label();
        let found = self.asm.new_label();
        self.asm.and_ri(RSI, 0xFF);
        self.asm.mov_rr(RAX, RDI);
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_reg_reg(RCX, RSI);
        self.asm.je(found);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(nf);
        self.asm.inc_r(RAX);
        self.asm.jmp(l);
        self.asm.place(nf);
        self.asm.mov_ri(RAX, 0);
        self.asm.place(found);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strlastchr(&mut self) {
        let l = self.asm.new_label();
        let skip = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.and_ri(RSI, 0xFF);
        self.asm.mov_ri(RAX, 0); // result = NULL
        self.asm.mov_rr(R8, RDI); // cursor
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, R8);
        self.asm.cmp_reg_reg(RCX, RSI);
        self.asm.jne(skip);
        self.asm.mov_rr(RAX, R8); // remember this match
        self.asm.place(skip);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(done);
        self.asm.inc_r(R8);
        self.asm.jmp(l);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strfind(&mut self) {
        let outer = self.asm.new_label();
        let inner = self.asm.new_label();
        let next = self.asm.new_label();
        let matched = self.asm.new_label();
        let nf = self.asm.new_label();
        // empty needle → return haystack
        self.asm.load_byte_zx(RCX, RSI);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(matched);
        self.asm.place(outer);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(nf); // haystack exhausted
        self.asm.mov_rr(R8, RDI); // haystack cursor
        self.asm.mov_rr(R9, RSI); // needle cursor
        self.asm.place(inner);
        self.asm.load_byte_zx(RCX, R9);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(matched); // needle exhausted → match at rdi
        self.asm.load_byte_zx(RDX, R8);
        self.asm.cmp_reg_reg(RCX, RDX);
        self.asm.jne(next);
        self.asm.inc_r(R8);
        self.asm.inc_r(R9);
        self.asm.jmp(inner);
        self.asm.place(next);
        self.asm.inc_r(RDI);
        self.asm.jmp(outer);
        self.asm.place(matched);
        self.asm.mov_rr(RAX, RDI);
        self.asm.emit(&[0xC3]);
        self.asm.place(nf);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]);
    }

    /// Shared body for `StrSpn`/`StrCSpn`: count the initial run of chars that are
    /// in (`want_in`) or not in (`!want_in`) the set. rdi=str, rsi=set → rax=len.
    fn emit_rt_strspn_impl(&mut self, want_in: bool) {
        let l = self.asm.new_label();
        let scan = self.asm.new_label();
        let cont = self.asm.new_label();
        let stop = self.asm.new_label();
        self.asm.mov_ri(RAX, 0);
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(stop); // end of string
        self.asm.mov_rr(R8, RSI); // set cursor
        self.asm.place(scan);
        self.asm.load_byte_zx(RDX, R8);
        self.asm.test_rr(RDX, RDX);
        // end of set: for StrSpn the char is NOT in the set (stop); for StrCSpn it
        // is not in the set (continue counting).
        if want_in {
            self.asm.je(stop);
        } else {
            self.asm.je(cont);
        }
        self.asm.cmp_reg_reg(RCX, RDX);
        if want_in {
            self.asm.je(cont); // found in set → keep counting
        } else {
            self.asm.je(stop); // found in set → stop
        }
        self.asm.inc_r(R8);
        self.asm.jmp(scan);
        self.asm.place(cont);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RAX);
        self.asm.jmp(l);
        self.asm.place(stop);
        self.asm.emit(&[0xC3]);
    }
    fn emit_rt_strspn(&mut self) {
        self.emit_rt_strspn_impl(true);
    }
    fn emit_rt_strcspn(&mut self) {
        self.emit_rt_strspn_impl(false);
    }

    /// `StrToUpper`/`StrToLower` (rdi=str → rax=str): ASCII-case in place.
    fn emit_rt_strcase(&mut self, upper: bool) {
        let l = self.asm.new_label();
        let skip = self.asm.new_label();
        let done = self.asm.new_label();
        let (lo, hi, delta) = if upper {
            (b'a', b'z', -32)
        } else {
            (b'A', b'Z', 32)
        };
        self.asm.mov_rr(RAX, RDI); // return str
        self.asm.place(l);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(done);
        self.asm.cmp_ri(RCX, lo as i32);
        self.asm.jl(skip);
        self.asm.cmp_ri(RCX, hi as i32);
        self.asm.jg(skip);
        self.asm.add_ri(RCX, delta);
        self.asm.store_byte_at(RDI, RCX);
        self.asm.place(skip);
        self.asm.inc_r(RDI);
        self.asm.jmp(l);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_strrev(&mut self) {
        let fe = self.asm.new_label();
        let ge = self.asm.new_label();
        let loop_l = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return str
        self.asm.mov_rr(R8, RDI); // left
        self.asm.mov_rr(R9, RDI); // right (scans to NUL)
        self.asm.place(fe);
        self.asm.cmp_byte_imm_at(R9, 0);
        self.asm.je(ge);
        self.asm.inc_r(R9);
        self.asm.jmp(fe);
        self.asm.place(ge);
        self.asm.place(loop_l);
        self.asm.dec_r(R9); // move right toward the start
        self.asm.cmp_reg_reg(R8, R9);
        self.asm.jae(done); // left >= right → done
        self.asm.load_byte_zx(RCX, R8);
        self.asm.load_byte_zx(RDX, R9);
        self.asm.store_byte_at(R8, RDX);
        self.asm.store_byte_at(R9, RCX);
        self.asm.inc_r(R8);
        self.asm.jmp(loop_l);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memcpy(&mut self) {
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.mov_rr(RCX, RDX); // count
        self.asm.rep_movsb();
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memmove(&mut self) {
        let fwd = self.asm.new_label();
        let done = self.asm.new_label();
        let bloop = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.cmp_reg_reg(RDI, RSI);
        self.asm.jb(fwd); // dst < src → forward copy is safe
        self.asm.je(done); // dst == src → nothing to do
        // backward copy: from the high end down
        self.asm.mov_rr(RCX, RDX); // counter
        self.asm.mov_rr(R8, RDI);
        self.asm.add_rr(R8, RDX); // r8 = dst + n
        self.asm.mov_rr(R9, RSI);
        self.asm.add_rr(R9, RDX); // r9 = src + n
        self.asm.place(bloop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(done);
        self.asm.dec_r(R8);
        self.asm.dec_r(R9);
        self.asm.load_byte_zx(RDX, R9);
        self.asm.store_byte_at(R8, RDX);
        self.asm.dec_r(RCX);
        self.asm.jmp(bloop);
        self.asm.place(fwd);
        self.asm.mov_rr(RCX, RDX);
        self.asm.rep_movsb();
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memset(&mut self) {
        let l = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI); // return dst
        self.asm.mov_rr(R8, RDI); // cursor
        self.asm.place(l);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(done);
        self.asm.store_byte_at(R8, RSI); // store low byte of c
        self.asm.inc_r(R8);
        self.asm.dec_r(RDX);
        self.asm.jmp(l);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memcmp(&mut self) {
        let l = self.asm.new_label();
        let diff = self.asm.new_label();
        let done = self.asm.new_label();
        let eq = self.asm.new_label();
        self.asm.place(l);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(eq);
        self.asm.load_byte_zx(RCX, RDI);
        self.asm.load_byte_zx(R8, RSI);
        self.asm.cmp_reg_reg(RCX, R8);
        self.asm.jne(diff);
        self.asm.inc_r(RDI);
        self.asm.inc_r(RSI);
        self.asm.dec_r(RDX);
        self.asm.jmp(l);
        self.asm.place(diff);
        self.asm.mov_ri(RAX, 1);
        self.asm.cmp_reg_reg(RCX, R8);
        self.asm.jg(done);
        self.asm.mov_ri(RAX, -1);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
        self.asm.place(eq);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memfind(&mut self) {
        let l = self.asm.new_label();
        let nf = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.and_ri(RSI, 0xFF);
        self.asm.mov_rr(RAX, RDI);
        self.asm.place(l);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(nf);
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_reg_reg(RCX, RSI);
        self.asm.je(done);
        self.asm.inc_r(RAX);
        self.asm.dec_r(RDX);
        self.asm.jmp(l);
        self.asm.place(nf);
        self.asm.mov_ri(RAX, 0);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_memsearch(&mut self) {
        let outer = self.asm.new_label();
        let inner = self.asm.new_label();
        let next = self.asm.new_label();
        let matched = self.asm.new_label();
        let nf = self.asm.new_label();
        // empty needle → return haystack
        self.asm.test_rr(RCX, RCX);
        self.asm.je(matched);
        // positions = hlen - nlen + 1 → r8 (in [1, ...]); negative ⇒ not found
        self.asm.mov_rr(R8, RSI);
        self.asm.sub_rr(R8, RCX);
        self.asm.js(nf);
        self.asm.inc_r(R8);
        self.asm.place(outer);
        self.asm.test_rr(R8, R8);
        self.asm.je(nf);
        self.asm.mov_rr(R9, RDI); // haystack cursor
        self.asm.mov_rr(R10, RDX); // needle cursor
        self.asm.mov_rr(R11, RCX); // needle remaining
        self.asm.place(inner);
        self.asm.test_rr(R11, R11);
        self.asm.je(matched);
        self.asm.load_byte_zx(RAX, R9);
        self.asm.load_byte_zx(RSI, R10);
        self.asm.cmp_reg_reg(RAX, RSI);
        self.asm.jne(next);
        self.asm.inc_r(R9);
        self.asm.inc_r(R10);
        self.asm.dec_r(R11);
        self.asm.jmp(inner);
        self.asm.place(next);
        self.asm.inc_r(RDI);
        self.asm.dec_r(R8);
        self.asm.jmp(outer);
        self.asm.place(matched);
        self.asm.mov_rr(RAX, RDI);
        self.asm.emit(&[0xC3]);
        self.asm.place(nf);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]);
    }

    /// `ToUpper`/`ToLower` (rdi=c → rax): ASCII-case a single character.
    fn emit_rt_tocase(&mut self, upper: bool) {
        let done = self.asm.new_label();
        let (lo, hi, delta) = if upper {
            (b'a', b'z', -32)
        } else {
            (b'A', b'Z', 32)
        };
        self.asm.mov_rr(RAX, RDI);
        self.asm.cmp_ri(RAX, lo as i32);
        self.asm.jl(done);
        self.asm.cmp_ri(RAX, hi as i32);
        self.asm.jg(done);
        self.asm.add_ri(RAX, delta);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_abs(&mut self) {
        let done = self.asm.new_label();
        self.asm.mov_rr(RAX, RDI);
        self.asm.test_rr(RAX, RAX);
        self.asm.jns(done);
        self.asm.neg_rax();
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    fn emit_rt_sign(&mut self) {
        let pos = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.mov_ri(RAX, 0);
        self.asm.test_rr(RDI, RDI);
        self.asm.jg(pos);
        self.asm.je(done);
        self.asm.mov_ri(RAX, -1);
        self.asm.emit(&[0xC3]);
        self.asm.place(pos);
        self.asm.mov_ri(RAX, 1);
        self.asm.place(done);
        self.asm.emit(&[0xC3]);
    }

    // ---- floating point (F64) ----

    /// Spill the current float result (xmm0) onto the machine stack, via a GPR.
    fn push_f(&mut self) {
        self.asm.movq_r_from_xmm(RAX, 0);
        self.asm.push_rax();
    }
    /// Pop a spilled float into `xmm` (the inverse of [`push_f`]).
    fn pop_f(&mut self, xmm: u8) {
        self.asm.pop_rax();
        self.asm.movq_xmm_from_r(xmm, RAX);
    }

    /// Evaluate `e` as a double in xmm0, converting from an integer if needed.
    fn gen_foperand(&mut self, e: &Expr) -> Result<(), CodegenError> {
        if is_f64(&self.expr_ty(e)) {
            self.gen_fexpr(e)
        } else {
            self.gen_expr(e)?; // integer in rax
            self.asm.cvtsi2sd(0, RAX);
            Ok(())
        }
    }

    /// Evaluate `e` for an integer-typed `target`. Identical to `gen_expr` except
    /// that an F64 source converting to an **unsigned** target uses the unsigned
    /// (`f as u64`) path rather than the default signed truncation.
    fn gen_int_expr(&mut self, e: &Expr, target: &Type) -> Result<(), CodegenError> {
        if is_f64(&self.expr_ty(e)) && is_unsigned_int(target) {
            self.gen_fexpr(e)?;
            self.gen_f64_to_u64();
            return Ok(());
        }
        self.gen_expr(e)
    }

    /// Convert the double in xmm0 to a u64 in rax, matching Rust's saturating
    /// `f as u64`: negatives/NaN → 0, and values ≥ 2^63 handled by a bias split
    /// (extreme saturation beyond 2^64 is not modelled — a documented edge).
    fn gen_f64_to_u64(&mut self) {
        let zero = self.asm.new_label();
        let big = self.asm.new_label();
        let done = self.asm.new_label();
        // value < 0 or NaN ⇒ 0
        self.asm.mov_rax_imm(0);
        self.asm.movq_xmm_from_r(1, RAX); // xmm1 = 0.0
        self.asm.ucomisd(0, 1);
        self.asm.jb(zero); // value < 0 (NaN also sets CF)
        self.asm.jp(zero); // unordered (NaN)
        // split at 2^63
        self.asm.mov_rax_imm(0x43E0_0000_0000_0000); // 2^63 as f64 bits
        self.asm.movq_xmm_from_r(1, RAX);
        self.asm.ucomisd(0, 1);
        self.asm.jae(big);
        self.asm.cvttsd2si(RAX, 0);
        self.asm.jmp(done);
        self.asm.place(big);
        self.asm.subsd(0, 1); // value - 2^63
        self.asm.cvttsd2si(RAX, 0);
        self.asm.btc_rax_63(); // + 2^63 (set bit 63)
        self.asm.jmp(done);
        self.asm.place(zero);
        self.asm.mov_rax_imm(0);
        self.asm.place(done);
    }

    /// Evaluate an F64-typed expression; the result lands in xmm0.
    fn gen_fexpr(&mut self, e: &Expr) -> Result<(), CodegenError> {
        let pos = e.span.pos;
        match &e.kind {
            ExprKind::Float(v) => {
                self.asm.mov_rax_imm(v.to_bits() as i64);
                self.asm.movq_xmm_from_r(0, RAX);
            }
            // An integer literal in float context (e.g. `F64 x = 5;`).
            ExprKind::Int(v) | ExprKind::Char(v) => {
                self.asm.mov_rax_imm(*v);
                self.asm.cvtsi2sd(0, RAX);
            }
            ExprKind::Ident(_)
            | ExprKind::Unary {
                op: UnOp::Deref, ..
            }
            | ExprKind::Index { .. }
            | ExprKind::Member { .. } => {
                self.gen_addr(e)?; // rax = address
                self.asm.movsd_load_at(RAX);
            }
            ExprKind::Unary {
                op: UnOp::Pos,
                expr,
            } => self.gen_fexpr(expr)?,
            ExprKind::Unary {
                op: UnOp::Neg,
                expr,
            } => {
                self.gen_fexpr(expr)?;
                self.asm.movq_r_from_xmm(RAX, 0);
                self.asm.btc_rax_63(); // flip the sign bit
                self.asm.movq_xmm_from_r(0, RAX);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                use BinOp::*;
                if !matches!(op, Add | Sub | Mul | Div) {
                    return Err(CodegenError::at(
                        pos,
                        format!("x86_64 backend: operator {op:?} is not supported on F64"),
                    ));
                }
                self.gen_foperand(lhs)?; // xmm0 = lhs
                self.push_f();
                self.gen_foperand(rhs)?; // xmm0 = rhs
                self.asm.movsd_rr(1, 0); // xmm1 = rhs
                self.pop_f(0); // xmm0 = lhs
                match op {
                    Add => self.asm.addsd(0, 1),
                    Sub => self.asm.subsd(0, 1),
                    Mul => self.asm.mulsd(0, 1),
                    Div => self.asm.divsd(0, 1),
                    _ => unreachable!(),
                }
            }
            ExprKind::Assign { op, target, value } => self.gen_fassign(*op, target, value, pos)?,
            ExprKind::Ternary { cond, then, else_ } => {
                let l_else = self.asm.new_label();
                let l_end = self.asm.new_label();
                self.gen_cond_jump_false(cond, l_else)?;
                self.gen_fexpr(then)?;
                self.asm.jmp(l_end);
                self.asm.place(l_else);
                self.gen_fexpr(else_)?;
                self.asm.place(l_end);
            }
            ExprKind::Cast { expr, .. } => {
                // Target is F64 (gen_fexpr is only entered for F64-typed exprs).
                self.gen_foperand(expr)?;
            }
            ExprKind::Call { callee, args } => {
                // A float-returning call leaves its result in xmm0.
                match &callee.kind {
                    ExprKind::Ident(n) => self.gen_call_by_name(n, args, pos)?,
                    _ => {
                        return Err(CodegenError::at(
                            pos,
                            "x86_64 backend: only direct function calls are supported yet",
                        ));
                    }
                }
            }
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: unsupported floating-point expression",
                ));
            }
        }
        Ok(())
    }

    /// Assignment whose target is F64; the stored value is left in xmm0.
    fn gen_fassign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        _pos: Pos,
    ) -> Result<(), CodegenError> {
        if op == AssignOp::Assign {
            self.gen_addr(target)?; // rax = &target
            self.asm.push_rax();
            self.gen_foperand(value)?; // xmm0 = value
            self.asm.pop_rcx();
            self.asm.movsd_store_at(RCX); // [target] = xmm0
            return Ok(());
        }
        // Compound `x op= value` on an F64 lvalue.
        let bop = compound_binop(op);
        if !matches!(bop, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
            return Err(CodegenError::at(
                _pos,
                format!("x86_64 backend: operator {bop:?} is not supported on F64"),
            ));
        }
        self.gen_addr(target)?; // rax = &target
        self.asm.push_rax();
        self.asm.movsd_load_at(RAX); // xmm0 = current
        self.push_f();
        self.gen_foperand(value)?; // xmm0 = value
        self.asm.movsd_rr(1, 0); // xmm1 = value
        self.pop_f(0); // xmm0 = current
        match bop {
            BinOp::Add => self.asm.addsd(0, 1),
            BinOp::Sub => self.asm.subsd(0, 1),
            BinOp::Mul => self.asm.mulsd(0, 1),
            BinOp::Div => self.asm.divsd(0, 1),
            _ => unreachable!(),
        }
        self.asm.pop_rcx(); // rcx = &target
        self.asm.movsd_store_at(RCX);
        Ok(())
    }

    // ---- printing ----

    /// Emit a printf-style print of the literal format `fmt` with `args`. Parses
    /// each `%[flags][width][.prec][len]conv` with the shared [`crate::fmt`] spec
    /// parser (so it agrees with the interpreter), then lowers integer conversions
    /// through the `fmt_int` runtime and string/char ones through `fmt_str`, both
    /// of which apply the flags/width/precision exactly as `fmt::render_*` does.
    fn gen_print(&mut self, fmt: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let mut chars = fmt.chars().peekable();
        let mut arg_i = 0;
        let mut lit: Vec<u8> = Vec::new();
        let next_arg = |args: &[Expr], ai: &mut usize| -> Result<usize, CodegenError> {
            let i = *ai;
            if i >= args.len() {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: too few arguments for format string",
                ));
            }
            *ai += 1;
            Ok(i)
        };
        while let Some(c) = chars.next() {
            if c != '%' {
                let mut b = [0u8; 4];
                lit.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                continue;
            }
            let spec = crate::fmt::parse(&mut chars);
            if spec.conv == '%' {
                lit.push(b'%');
                continue;
            }
            self.emit_literal(&lit);
            lit.clear();

            // `*` width / precision come from arguments, consumed (left to right)
            // before the value — push them now so they survive the value eval.
            if spec.width_star {
                let a = next_arg(args, &mut arg_i)?;
                self.gen_expr(&args[a])?;
                self.asm.push_rax();
            }
            if spec.prec_star {
                let a = next_arg(args, &mut arg_i)?;
                self.gen_expr(&args[a])?;
                self.asm.push_rax();
            }

            let mut flags = 0;
            if spec.minus {
                flags |= F_MINUS;
            }
            if spec.plus {
                flags |= F_PLUS;
            }
            if spec.space {
                flags |= F_SPACE;
            }
            if spec.zero {
                flags |= F_ZERO;
            }
            if spec.hash {
                flags |= F_HASH;
            }

            let a = next_arg(args, &mut arg_i)?;
            match spec.conv {
                'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                    let (radix, extra) = match spec.conv {
                        'd' | 'i' => (10, F_SIGNED),
                        'u' => (10, 0),
                        'x' => (16, 0),
                        'X' => (16, F_UPPER),
                        _ => (8, 0), // 'o'
                    };
                    self.gen_expr(&args[a])?;
                    self.asm.mov_rdi_rax(); // rdi = value
                    self.setup_width_prec_flags(&spec, flags | extra, false);
                    self.asm.mov_ri(RSI, radix);
                    let l = self.helper(Helper::FmtInt);
                    self.asm.call(l);
                }
                'c' => {
                    let cb = self.charbuf();
                    self.gen_expr(&args[a])?;
                    self.asm.lea_global(RCX, cb); // rcx = &charbuf
                    self.asm.store_byte_at(RCX, RAX); // charbuf[0] = low byte
                    self.asm.lea_global(RDI, cb); // rdi = &charbuf
                    self.setup_width_prec_flags(&spec, flags, true); // %c ignores precision
                    self.asm.mov_ri(RSI, 1); // len = 1
                    let l = self.helper(Helper::FmtStr);
                    self.asm.call(l);
                }
                's' => {
                    self.gen_expr(&args[a])?;
                    self.asm.mov_rdi_rax(); // rdi = string pointer
                    self.setup_width_prec_flags(&spec, flags, false);
                    self.asm.mov_ri(RSI, -1); // len = -1 ⇒ strlen
                    let l = self.helper(Helper::FmtStr);
                    self.asm.call(l);
                }
                'f' | 'e' | 'E' | 'g' | 'G' => {
                    self.gen_foperand(&args[a])?; // xmm0 = value (int → double)
                    self.setup_width_prec_flags(&spec, flags, false); // rcx=width, r8=prec, rdx=flags
                    // Float precision defaults to 6 when unspecified (libc/Rust rule).
                    let have_prec = self.asm.new_label();
                    self.asm.cmp_ri(R8, 0);
                    self.asm.jge(have_prec);
                    self.asm.mov_ri(R8, 6);
                    self.asm.place(have_prec);
                    self.asm.mov_rr(RSI, RDX); // esi = flags
                    self.asm.mov_rr(RDI, R8); //  edi = precision
                    self.asm.mov_rr(RDX, RCX); // edx = width
                    let (h, conv) = match spec.conv {
                        'f' => (Helper::FmtFloat, 0),
                        'e' => (Helper::FmtFloatEg, 1),
                        'E' => (Helper::FmtFloatEg, 1 | 4),
                        'g' => (Helper::FmtFloatEg, 2),
                        _ => (Helper::FmtFloatEg, 2 | 4), // 'G'
                    };
                    self.asm.mov_ri(RCX, conv); // ecx = conv (FmtFloatEg only)
                    let l = self.helper(h);
                    self.asm.call(l);
                }
                other => {
                    return Err(CodegenError::at(
                        pos,
                        format!("x86_64 backend: unsupported format conversion %{other}"),
                    ));
                }
            }
        }
        self.emit_literal(&lit);
        Ok(())
    }

    /// After the value argument is in place, set `r8`=precision, `rcx`=width, and
    /// `rdx`=flags for a `fmt_int`/`fmt_str` call, consuming any pushed `*` args.
    /// `force_prec_none` (for `%c`) discards a precision but still pops a `*` arg.
    fn setup_width_prec_flags(
        &mut self,
        spec: &crate::fmt::Spec,
        flags: i32,
        force_prec_none: bool,
    ) {
        // Precision into r8 (−1 ⇒ none). A `*` precision was pushed after width.
        if spec.prec_star {
            self.asm.pop_rax();
            if force_prec_none {
                self.asm.mov_ri(R8, -1);
            } else {
                // A negative `*` precision means "no precision" (libc/interp rule).
                self.asm.mov_rr(R8, RAX);
                let neg = self.asm.new_label();
                let done = self.asm.new_label();
                self.asm.test_rr(R8, R8);
                self.asm.js(neg);
                self.asm.jmp(done);
                self.asm.place(neg);
                self.asm.mov_ri(R8, -1);
                self.asm.place(done);
            }
        } else if spec.has_precision && !force_prec_none {
            self.asm.mov_ri(R8, spec.precision as i32);
        } else {
            self.asm.mov_ri(R8, -1);
        }
        // Width into rcx, flags into rdx. A negative `*` width ⇒ left-justify + abs.
        if spec.width_star {
            self.asm.pop_rax();
            self.asm.mov_rr(RCX, RAX);
            self.asm.mov_ri(RDX, flags);
            let pos_l = self.asm.new_label();
            self.asm.test_rr(RCX, RCX);
            self.asm.jns(pos_l);
            self.asm.neg_r(RCX);
            self.asm.or_ri(RDX, F_MINUS);
            self.asm.place(pos_l);
        } else {
            self.asm.mov_ri(RCX, spec.width.unwrap_or(0) as i32);
            self.asm.mov_ri(RDX, flags);
        }
    }

    /// Write a literal byte run (if non-empty): `lea rsi, [str]; mov rdx, len;
    /// write(1, rsi, rdx)`.
    fn emit_literal(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let idx = self.asm.intern(bytes);
        self.asm.lea_rsi_string(idx);
        self.asm.mov_rdx_imm32(bytes.len() as i32);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
    }

    /// Emit the bodies of the print-runtime helpers actually used.
    fn emit_helpers(&mut self) {
        if let Some(&label) = self.helpers.get(&Helper::FmtInt) {
            self.asm.place(label);
            self.emit_fmt_int();
        }
        if let Some(&label) = self.helpers.get(&Helper::FmtStr) {
            self.asm.place(label);
            self.emit_fmt_str();
        }
        if let Some(&label) = self.helpers.get(&Helper::OutWrite) {
            self.asm.place(label);
            self.emit_out_write();
        }
        if let Some(&label) = self.helpers.get(&Helper::GrowSink) {
            self.asm.place(label);
            self.emit_grow_sink();
        }
        // FmtFloat first: emitting it registers the bignum sub-routines it calls,
        // which are then emitted below (label references resolve at `finish`).
        for (h, emit) in [
            (Helper::FmtFloat, Self::emit_fmt_float as fn(&mut Self)),
            (Helper::FmtFloatEg, Self::emit_fmt_float_eg),
            (Helper::BnMul, Self::emit_bn_mul),
            (Helper::BnShr, Self::emit_bn_shr),
            (Helper::BnShl, Self::emit_bn_shl),
            (Helper::BnDiv10, Self::emit_bn_div10),
        ] {
            if let Some(&label) = self.helpers.get(&h) {
                self.asm.place(label);
                emit(self);
            }
        }
    }

    /// The output sink routine (`rsi`=buf, `rdx`=len). To stdout when `out_ptr` is
    /// 0; otherwise it appends to the destination buffer and advances `out_ptr`.
    /// When the buffer is an owned, growing `MStrPrint` one (`out_limit != 0`), it
    /// first grows via [`Helper::GrowSink`] if the write would overflow.
    fn emit_out_write(&mut self) {
        let off = self.out_ptr_global();
        let stdout = self.asm.new_label();
        let copy = self.asm.new_label();
        self.asm.lea_global(R8, off);
        self.asm.load_qword_at(RAX, R8); // rax = out_ptr (cursor)
        self.asm.test_rr(RAX, RAX);
        self.asm.je(stdout);
        // Owned/growing buffer? Grow if cursor + len would leave no room for a NUL.
        if self.out_limit_off.is_some() {
            let limit = self.out_limit_global();
            self.asm.lea_global(R9, limit);
            self.asm.load_qword_at(R9, R9); // r9 = out_limit
            self.asm.test_rr(R9, R9);
            self.asm.je(copy); // limit == 0 → plain StrPrint append, no grow
            self.asm.mov_rr(RCX, RAX);
            self.asm.add_rr(RCX, RDX); // rcx = cursor + len
            self.asm.cmp_rr(RCX, R9);
            self.asm.jb(copy); // cursor + len < limit → fits (NUL has room)
            let grow = self.helper(Helper::GrowSink);
            self.asm.call(grow); // returns the (possibly new) cursor in rax
        }
        // buffer mode: memcpy len bytes [rsi]->[rax]; out_ptr = rax + len
        self.asm.place(copy);
        self.asm.mov_rr(RDI, RAX);
        self.asm.mov_rr(RCX, RDX);
        self.asm.rep_movsb(); // advances rdi by len
        self.asm.lea_global(R8, off); // (GrowSink may have clobbered r8)
        self.asm.store_qword_at(R8, RDI); // out_ptr = new position
        self.asm.emit(&[0xC3]); // ret
        self.asm.place(stdout);
        self.os.emit_write_stdout(&mut self.asm); // write rdx bytes at rsi to stdout
        self.asm.emit(&[0xC3]); // ret
    }

    /// [`Helper::GrowSink`]: reallocate the owned `MStrPrint` buffer so a pending
    /// `rdx`-byte write (plus a trailing NUL) fits. New capacity is `max(2·old,
    /// used + len + 1)`. Copies the live bytes, updates `out_base`/`out_ptr`/
    /// `out_limit`, and returns the new cursor in `rax`. Preserves `rsi`/`rdx` (the
    /// sink still needs the pending write); other volatiles are scratch. The bump
    /// allocator never frees, so the old buffer is simply abandoned.
    fn emit_grow_sink(&mut self) {
        let out = self.out_ptr_global();
        let base = self.out_base_global();
        let limit = self.out_limit_global();
        self.asm.emit(&[0x56]); // push rsi
        self.asm.emit(&[0x52]); // push rdx
        // new_cap (r9) = max(2·oldcap, used + len + 1)
        self.asm.lea_global(R8, base);
        self.asm.load_qword_at(R10, R8); // r10 = base
        self.asm.lea_global(R8, out);
        self.asm.load_qword_at(R11, R8); // r11 = cursor
        self.asm.sub_rr(R11, R10); // r11 = used = cursor - base
        self.asm.add_rr(R11, RDX); // r11 = used + len
        self.asm.add_ri(R11, 1); // r11 = used + len + 1 (need)
        self.asm.lea_global(R8, limit);
        self.asm.load_qword_at(R9, R8); // r9 = limit
        self.asm.sub_rr(R9, R10); // r9 = oldcap = limit - base
        self.asm.add_rr(R9, R9); // r9 = 2·oldcap
        let have = self.asm.new_label();
        self.asm.cmp_rr(R9, R11);
        self.asm.jae(have); // 2·oldcap >= need
        self.asm.mov_rr(R9, R11); // else new_cap = need
        self.asm.place(have);
        // newbuf = MAlloc(new_cap)
        self.asm.emit(&[0x41, 0x51]); // push r9 (new_cap)
        self.asm.mov_rr(RDI, R9);
        let malloc = self.rt_routine("MAlloc");
        self.asm.call(malloc); // rax = newbuf
        self.asm.emit(&[0x41, 0x59]); // pop r9 (new_cap)
        self.asm.mov_rr(R11, RAX); // r11 = newbuf
        // Copy the live bytes [base .. cursor) into newbuf (rep movsb leaves rdi at
        // newbuf + used = the new cursor).
        self.asm.lea_global(R8, base);
        self.asm.load_qword_at(RSI, R8); // rsi = old base (src)
        self.asm.lea_global(R8, out);
        self.asm.load_qword_at(RCX, R8); // rcx = cursor
        self.asm.sub_rr(RCX, RSI); // rcx = used
        self.asm.mov_rr(RDI, R11); // rdi = newbuf (dst)
        self.asm.rep_movsb(); // rdi = newbuf + used
        // Publish the new buffer: base = newbuf, cursor = rdi, limit = newbuf + cap.
        self.asm.lea_global(R8, base);
        self.asm.store_qword_at(R8, R11);
        self.asm.lea_global(R8, out);
        self.asm.store_qword_at(R8, RDI);
        self.asm.mov_rr(RAX, R11);
        self.asm.add_rr(RAX, R9); // rax = newbuf + new_cap
        self.asm.lea_global(R8, limit);
        self.asm.store_qword_at(R8, RAX);
        self.asm.mov_rr(RAX, RDI); // return the new cursor
        self.asm.emit(&[0x5A]); // pop rdx
        self.asm.emit(&[0x5E]); // pop rsi
        self.asm.emit(&[0xC3]); // ret
    }

    /// `ret` after restoring the registers the formatters borrow. Paired with
    /// `fmt_prologue` (push rbx, r12–r15) so the helpers preserve callee-saved regs.
    fn fmt_prologue(&mut self) {
        self.asm.emit(&[0x53]); // push rbx
        self.asm.emit(&[0x41, 0x54]); // push r12
        self.asm.emit(&[0x41, 0x55]); // push r13
        self.asm.emit(&[0x41, 0x56]); // push r14
        self.asm.emit(&[0x41, 0x57]); // push r15
    }
    fn fmt_epilogue(&mut self) {
        self.asm.emit(&[0x41, 0x5F]); // pop r15
        self.asm.emit(&[0x41, 0x5E]); // pop r14
        self.asm.emit(&[0x41, 0x5D]); // pop r13
        self.asm.emit(&[0x41, 0x5C]); // pop r12
        self.asm.emit(&[0x5B]); // pop rbx
        self.asm.emit(&[0xC3]); // ret
    }

    /// Append the sign byte (in `r9b`, 0 ⇒ none) to the OUTBUF cursor `r14`.
    fn fmt_append_sign(&mut self) {
        let skip = self.asm.new_label();
        self.asm.test_rr(R9, R9);
        self.asm.je(skip);
        self.asm.store_byte_at(R14, R9);
        self.asm.inc_r(R14);
        self.asm.place(skip);
    }
    /// Append the alt prefix (`0x`/`0X`) when `rcx` (alt length) is 2.
    fn fmt_append_alt(&mut self) {
        let skip = self.asm.new_label();
        let wrote = self.asm.new_label();
        self.asm.test_rr(RCX, RCX);
        self.asm.je(skip);
        self.asm.store_byte_imm_at(R14, b'0');
        self.asm.inc_r(R14);
        self.asm.mov_ri(RAX, b'x' as i32);
        self.asm.test_ri(R11, F_UPPER);
        self.asm.je(wrote);
        self.asm.mov_ri(RAX, b'X' as i32);
        self.asm.place(wrote);
        self.asm.store_byte_at(R14, RAX);
        self.asm.inc_r(R14);
        self.asm.place(skip);
    }
    /// Append the `rbx` digits at `r15` to the OUTBUF cursor `r14` (`rep movsb`).
    fn fmt_append_digits(&mut self) {
        self.asm.mov_rr(RSI, R15);
        self.asm.mov_rr(RDI, R14);
        self.asm.mov_rr(RCX, RBX);
        self.asm.rep_movsb();
        self.asm.mov_rr(R14, RDI); // cursor advanced by the copy
    }
    /// Append `rdx` copies of byte `fill` to the OUTBUF cursor `r14`.
    fn fmt_append_pad(&mut self, fill: u8) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.place(top);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(done);
        self.asm.store_byte_imm_at(R14, fill);
        self.asm.inc_r(R14);
        self.asm.dec_r(RDX);
        self.asm.jmp(top);
        self.asm.place(done);
    }

    /// The integer formatter — mirrors `fmt::render_int` byte for byte. Builds the
    /// padded field in OUTBUF (sign, `0x` prefix, precision/zero padding, digits,
    /// space padding) and writes it once. See [`Helper::FmtInt`] for the ABI.
    fn emit_fmt_int(&mut self) {
        let digbuf = self.digbuf();
        let outbuf = self.outbuf();
        self.fmt_prologue();
        // Stash the register arguments into callee-saved slots.
        self.asm.mov_rr(R10, RSI); // radix
        self.asm.mov_rr(R11, RDX); // flags
        self.asm.mov_rr(R12, RCX); // width
        self.asm.mov_rr(R13, R8); // precision
        // ---- derive the sign char (r9) and magnitude (rdi) ----
        let unsigned = self.asm.new_label();
        let havesign = self.asm.new_label();
        let nonneg = self.asm.new_label();
        let tryspace = self.asm.new_label();
        self.asm.mov_ri(R9, 0);
        self.asm.test_ri(R11, F_SIGNED);
        self.asm.je(unsigned);
        self.asm.test_rr(RDI, RDI);
        self.asm.jns(nonneg);
        self.asm.neg_r(RDI);
        self.asm.mov_ri(R9, b'-' as i32);
        self.asm.jmp(havesign);
        self.asm.place(nonneg);
        self.asm.test_ri(R11, F_PLUS);
        self.asm.je(tryspace);
        self.asm.mov_ri(R9, b'+' as i32);
        self.asm.jmp(havesign);
        self.asm.place(tryspace);
        self.asm.test_ri(R11, F_SPACE);
        self.asm.je(havesign);
        self.asm.mov_ri(R9, b' ' as i32);
        self.asm.place(havesign);
        self.asm.place(unsigned);
        // ---- convert the magnitude to digits in DIGBUF (right to left) ----
        let dloop = self.asm.new_label();
        let store = self.asm.new_label();
        let upper = self.asm.new_label();
        self.asm.lea_global(R15, digbuf + FS_INT_DIGBUF); // one past the buffer end
        self.asm.mov_rr(RAX, RDI);
        self.asm.place(dloop);
        self.asm.xor_rr(RDX, RDX);
        self.asm.div_r(R10); // rax = quot, rdx = rem
        self.asm.add_ri(RDX, b'0' as i32);
        self.asm.cmp_ri(RDX, b'9' as i32);
        self.asm.jbe(store);
        self.asm.test_ri(R11, F_UPPER);
        self.asm.jne(upper);
        self.asm.add_ri(RDX, 0x27); // 'a'..'f'
        self.asm.jmp(store);
        self.asm.place(upper);
        self.asm.add_ri(RDX, 0x07); // 'A'..'F'
        self.asm.place(store);
        self.asm.dec_r(R15);
        self.asm.store_byte_at(R15, RDX);
        self.asm.test_rr(RAX, RAX);
        self.asm.jne(dloop);
        self.asm.lea_global(RBX, digbuf + FS_INT_DIGBUF);
        self.asm.sub_rr(RBX, R15); // rbx = digit count
        // ---- octal `#`: ensure a leading 0 ----
        let nooct = self.asm.new_label();
        self.asm.cmp_ri(R10, 8);
        self.asm.jne(nooct);
        self.asm.test_ri(R11, F_HASH);
        self.asm.je(nooct);
        self.asm.cmp_byte_imm_at(R15, b'0');
        self.asm.je(nooct);
        self.asm.dec_r(R15);
        self.asm.store_byte_imm_at(R15, b'0');
        self.asm.inc_r(RBX);
        self.asm.place(nooct);
        // ---- precision (min digits); 0 value with precision 0 ⇒ no digits ----
        let precdone = self.asm.new_label();
        let precpad = self.asm.new_label();
        let ploop = self.asm.new_label();
        self.asm.cmp_ri(R13, 0);
        self.asm.jl(precdone); // precision −1 ⇒ none
        self.asm.jne(precpad);
        // octal `#` keeps a leading 0 even at precision 0 of value 0 (don't drop).
        let not_octhash = self.asm.new_label();
        self.asm.cmp_ri(R10, 8);
        self.asm.jne(not_octhash);
        self.asm.test_ri(R11, F_HASH);
        self.asm.jne(precdone);
        self.asm.place(not_octhash);
        self.asm.cmp_ri(RBX, 1);
        self.asm.jne(precpad);
        self.asm.cmp_byte_imm_at(R15, b'0');
        self.asm.jne(precpad);
        self.asm.inc_r(R15); // drop the single '0'
        self.asm.xor_rr(RBX, RBX);
        self.asm.jmp(precdone);
        self.asm.place(precpad);
        self.asm.place(ploop);
        self.asm.cmp_rr(RBX, R13);
        self.asm.jge(precdone);
        self.asm.dec_r(R15);
        self.asm.store_byte_imm_at(R15, b'0');
        self.asm.inc_r(RBX);
        self.asm.jmp(ploop);
        self.asm.place(precdone);
        // ---- alt length (rcx): `0x`/`0X` for `#` hex of a non-zero value ----
        let noalt = self.asm.new_label();
        self.asm.xor_rr(RCX, RCX);
        self.asm.test_ri(R11, F_HASH);
        self.asm.je(noalt);
        self.asm.cmp_ri(R10, 16);
        self.asm.jne(noalt);
        self.asm.test_rr(RDI, RDI);
        self.asm.je(noalt);
        self.asm.mov_ri(RCX, 2);
        self.asm.place(noalt);
        // ---- assemble into OUTBUF ----
        self.asm.lea_global(R14, outbuf); // cursor
        // body_len = digits + alt + (sign ? 1 : 0)  → rax
        let nosl = self.asm.new_label();
        self.asm.mov_rr(RAX, RBX);
        self.asm.add_rr(RAX, RCX);
        self.asm.test_rr(R9, R9);
        self.asm.je(nosl);
        self.asm.inc_r(RAX);
        self.asm.place(nosl);
        // pad = max(0, width - body_len)  → rdx
        let padok = self.asm.new_label();
        self.asm.mov_rr(RDX, R12);
        self.asm.sub_rr(RDX, RAX);
        self.asm.jns(padok);
        self.asm.xor_rr(RDX, RDX);
        self.asm.place(padok);
        // choose justification
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let donebody = self.asm.new_label();
        self.asm.test_ri(R11, F_MINUS);
        self.asm.jne(do_minus);
        self.asm.test_ri(R11, F_ZERO);
        self.asm.je(do_right);
        self.asm.cmp_ri(R13, 0); // zero flag ignored when precision is given
        self.asm.jge(do_right);
        // zero-justify: sign, alt, zeros, digits
        self.fmt_append_sign();
        self.fmt_append_alt();
        self.fmt_append_pad(b'0');
        self.fmt_append_digits();
        self.asm.jmp(donebody);
        // right-justify: spaces, sign, alt, digits
        self.asm.place(do_right);
        self.fmt_append_pad(b' ');
        self.fmt_append_sign();
        self.fmt_append_alt();
        self.fmt_append_digits();
        self.asm.jmp(donebody);
        // left-justify: sign, alt, digits, spaces
        self.asm.place(do_minus);
        self.fmt_append_sign();
        self.fmt_append_alt();
        self.fmt_append_digits();
        self.fmt_append_pad(b' ');
        self.asm.place(donebody);
        // output OUTBUF[0 .. cursor-OUTBUF] through the sink
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, R14);
        self.asm.sub_rr(RDX, RSI);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
        self.fmt_epilogue();
    }

    /// The string/char formatter — mirrors `fmt::render_str`. Applies a precision
    /// (truncate) then pads to width (left-justified with `-`). See [`Helper::FmtStr`].
    fn emit_fmt_str(&mut self) {
        self.fmt_prologue();
        self.asm.mov_rr(R10, RDI); // ptr
        self.asm.mov_rr(R11, RSI); // len
        self.asm.mov_rr(R12, RDX); // flags
        self.asm.mov_rr(R13, RCX); // width
        // r8 = precision
        // ---- len: if negative, strlen(ptr) ----
        let havelen = self.asm.new_label();
        let slloop = self.asm.new_label();
        let sldone = self.asm.new_label();
        self.asm.cmp_ri(R11, 0);
        self.asm.jge(havelen);
        self.asm.mov_rr(RAX, R10);
        self.asm.place(slloop);
        self.asm.cmp_byte_imm_at(RAX, 0);
        self.asm.je(sldone);
        self.asm.inc_r(RAX);
        self.asm.jmp(slloop);
        self.asm.place(sldone);
        self.asm.sub_rr(RAX, R10);
        self.asm.mov_rr(R11, RAX);
        self.asm.place(havelen);
        // ---- precision: clamp len to precision when 0 ≤ prec < len ----
        let noprec = self.asm.new_label();
        self.asm.cmp_ri(R8, 0);
        self.asm.jl(noprec);
        self.asm.cmp_rr(R8, R11);
        self.asm.jge(noprec);
        self.asm.mov_rr(R11, R8);
        self.asm.place(noprec);
        // ---- pad = max(0, width - len)  → r15 (survives the body write) ----
        let padok = self.asm.new_label();
        self.asm.mov_rr(R15, R13);
        self.asm.sub_rr(R15, R11);
        self.asm.jns(padok);
        self.asm.xor_rr(R15, R15);
        self.asm.place(padok);
        // ---- emit: minus ⇒ body then pad; else pad then body ----
        let do_minus = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.test_ri(R12, F_MINUS);
        self.asm.jne(do_minus);
        self.fmt_str_pad();
        self.fmt_str_body();
        self.asm.jmp(done);
        self.asm.place(do_minus);
        self.fmt_str_body();
        self.fmt_str_pad();
        self.asm.place(done);
        self.fmt_epilogue();
    }

    /// Write the `fmt_str` body (pointer r10, length r11) through the sink.
    fn fmt_str_body(&mut self) {
        self.asm.mov_rr(RSI, R10);
        self.asm.mov_rr(RDX, R11);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
    }
    /// Write `r15` space characters to stdout, in OUTBUF-sized chunks.
    fn fmt_str_pad(&mut self) {
        let outbuf = self.outbuf();
        let outer = self.asm.new_label();
        let done = self.asm.new_label();
        let noclamp = self.asm.new_label();
        let fill = self.asm.new_label();
        let filled = self.asm.new_label();
        self.asm.place(outer);
        self.asm.test_rr(R15, R15);
        self.asm.je(done);
        // chunk = min(r15, OUTBUF_SIZE) → rbx
        self.asm.mov_rr(RBX, R15);
        self.asm.cmp_ri(RBX, 1024);
        self.asm.jbe(noclamp);
        self.asm.mov_ri(RBX, 1024);
        self.asm.place(noclamp);
        self.asm.sub_rr(R15, RBX); // remaining -= chunk
        // fill OUTBUF[0..chunk] with spaces
        self.asm.lea_global(R14, outbuf);
        self.asm.mov_rr(RCX, RBX);
        self.asm.place(fill);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(filled);
        self.asm.store_byte_imm_at(R14, b' ');
        self.asm.inc_r(R14);
        self.asm.dec_r(RCX);
        self.asm.jmp(fill);
        self.asm.place(filled);
        // output OUTBUF[0 .. chunk] through the sink
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, RBX);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
        self.asm.jmp(outer);
        self.asm.place(done);
    }

    // ---- float printing: a correctly-rounded `%f` via bignum arithmetic ----
    //
    // For a magnitude m·2^e and precision P, the exact rounded integer
    // J = round_half_even(m·2^e·10^P) (10^P = 2^P·5^P) is built in the `BIGNUM`
    // scratch: set it to m, multiply by 5 P times, then shift left by e+P (or
    // right, with round-to-even, when e+P<0). J's decimal digits with the point P
    // places from the right give the byte-exact `%f` body (matching Rust's `{:.P}`).

    /// `BIGNUM *= rdi` (a small multiplier).
    fn emit_bn_mul(&mut self) {
        let bn = self.bignum_global();
        let loop_l = self.asm.new_label();
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(R9, 0); // carry
        self.asm.mov_ri(R10, 0); // index
        self.asm.place(loop_l);
        self.asm.load_qword_idx8(RAX, R8, R10);
        self.asm.mul_r(RDI); // rdx:rax = limb * k
        self.asm.add_rr(RAX, R9);
        self.asm.adc_ri8(RDX, 0);
        self.asm.store_qword_idx8(R8, R10, RAX);
        self.asm.mov_rr(R9, RDX); // new carry
        self.asm.inc_r(R10);
        self.asm.cmp_ri(R10, NLIMBS);
        self.asm.jb(loop_l);
        self.asm.emit(&[0xC3]);
    }

    /// `BIGNUM /= 10`, returning the remainder digit in rax.
    fn emit_bn_div10(&mut self) {
        let bn = self.bignum_global();
        let loop_l = self.asm.new_label();
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(R10, NLIMBS - 1); // process most-significant limb first
        self.asm.mov_ri(RDX, 0); // running remainder
        self.asm.mov_ri(R9, 10);
        self.asm.place(loop_l);
        self.asm.load_qword_idx8(RAX, R8, R10);
        self.asm.div_r(R9); // rdx:rax / 10 → rax quot, rdx rem
        self.asm.store_qword_idx8(R8, R10, RAX);
        self.asm.dec_r(R10);
        self.asm.jns(loop_l);
        self.asm.mov_rr(RAX, RDX);
        self.asm.emit(&[0xC3]);
    }

    /// `BIGNUM <<= rdi` (bits).
    fn emit_bn_shl(&mut self) {
        let bn = self.bignum_global();
        let loop_l = self.asm.new_label();
        let zero = self.asm.new_label();
        let store = self.asm.new_label();
        self.asm.mov_rr(RSI, RDI);
        self.asm.shr_ri(RSI, 6); // word = bits/64
        self.asm.mov_rr(R11, RDI);
        self.asm.and_ri(R11, 63); // bit = bits%64
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(RDI, NLIMBS - 1); // i (high → low)
        self.asm.place(loop_l);
        self.asm.mov_rr(R10, RDI);
        self.asm.sub_rr(R10, RSI); // src = i - word
        self.asm.js(zero);
        self.asm.load_qword_idx8(RAX, R8, R10); // lo
        self.asm.test_rr(R11, R11);
        self.asm.je(store); // bit==0 → just lo
        self.asm.mov_rr(RCX, R11);
        self.asm.shl_cl(RAX); // lo << bit
        self.asm.dec_r(R10); // src-1
        self.asm.js(store);
        self.asm.load_qword_idx8(RDX, R8, R10); // hi
        self.asm.mov_ri(RCX, 64);
        self.asm.sub_rr(RCX, R11);
        self.asm.shr_cl(RDX); // hi >> (64-bit)
        self.asm.or_rr(RAX, RDX);
        self.asm.jmp(store);
        self.asm.place(zero);
        self.asm.mov_ri(RAX, 0);
        self.asm.place(store);
        self.asm.store_qword_idx8(R8, RDI, RAX);
        self.asm.dec_r(RDI);
        self.asm.jns(loop_l);
        self.asm.emit(&[0xC3]);
    }

    /// `BIGNUM >>= rdi` (bits), rounding the dropped bits to nearest, ties to even.
    fn emit_bn_shr(&mut self) {
        let bn = self.bignum_global();
        self.asm.emit(&[0x53, 0x41, 0x54, 0x41, 0x55]); // push rbx; push r12; push r13
        self.asm.lea_global(R12, bn);
        self.asm.mov_rr(RBX, RDI); // bits
        // --- round bit (r13) and sticky (r9) of the dropped low `bits` bits ---
        let sloop = self.asm.new_label();
        let sdone = self.asm.new_label();
        self.asm.mov_rr(RAX, RBX);
        self.asm.add_ri(RAX, -1); // m = bits-1
        self.asm.mov_rr(R10, RAX);
        self.asm.shr_ri(R10, 6); // mword
        self.asm.mov_rr(RSI, RAX);
        self.asm.and_ri(RSI, 63); // mbit
        self.asm.load_qword_idx8(RAX, R12, R10); // limb[mword]
        self.asm.mov_rr(R9, RAX); // copy for sticky-partial
        self.asm.mov_rr(RCX, RSI);
        self.asm.shr_cl(RAX);
        self.asm.and_ri(RAX, 1);
        self.asm.mov_rr(R13, RAX); // round bit
        self.asm.mov_ri(R11, 1);
        self.asm.mov_rr(RCX, RSI);
        self.asm.shl_cl(R11);
        self.asm.add_ri(R11, -1); // mask = (1<<mbit)-1
        self.asm.and_rr(R9, R11); // partial low bits of limb[mword]
        self.asm.mov_ri(RDI, 0); // j
        self.asm.place(sloop);
        self.asm.cmp_reg_reg(RDI, R10);
        self.asm.jae(sdone);
        self.asm.load_qword_idx8(RAX, R12, RDI);
        self.asm.or_rr(R9, RAX);
        self.asm.inc_r(RDI);
        self.asm.jmp(sloop);
        self.asm.place(sdone);
        // --- shift right by `bits` (word = bits/64, bit = bits%64) ---
        let shloop = self.asm.new_label();
        let lozero = self.asm.new_label();
        let havelo = self.asm.new_label();
        let store2 = self.asm.new_label();
        self.asm.mov_rr(RSI, RBX);
        self.asm.shr_ri(RSI, 6); // word
        self.asm.mov_rr(R11, RBX);
        self.asm.and_ri(R11, 63); // bit
        self.asm.mov_ri(RDI, 0); // i (low → high)
        self.asm.place(shloop);
        self.asm.mov_rr(R10, RDI);
        self.asm.add_rr(R10, RSI); // src = i + word
        self.asm.cmp_ri(R10, NLIMBS);
        self.asm.jae(lozero);
        self.asm.load_qword_idx8(RAX, R12, R10);
        self.asm.jmp(havelo);
        self.asm.place(lozero);
        self.asm.mov_ri(RAX, 0);
        self.asm.place(havelo);
        self.asm.test_rr(R11, R11);
        self.asm.je(store2);
        self.asm.mov_rr(RCX, R11);
        self.asm.shr_cl(RAX); // lo >> bit
        self.asm.inc_r(R10); // src+1
        self.asm.cmp_ri(R10, NLIMBS);
        self.asm.jae(store2);
        self.asm.load_qword_idx8(RDX, R12, R10);
        self.asm.mov_ri(RCX, 64);
        self.asm.sub_rr(RCX, R11);
        self.asm.shl_cl(RDX); // hi << (64-bit)
        self.asm.or_rr(RAX, RDX);
        self.asm.place(store2);
        self.asm.store_qword_idx8(R12, RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.cmp_ri(RDI, NLIMBS);
        self.asm.jb(shloop);
        // --- round up if round_bit && (sticky || quotient is odd) ---
        let done = self.asm.new_label();
        let roundup = self.asm.new_label();
        let iloop = self.asm.new_label();
        self.asm.test_rr(R13, R13);
        self.asm.je(done);
        self.asm.test_rr(R9, R9);
        self.asm.jne(roundup);
        self.asm.mov_ri(RDI, 0); // [r12] needs the SIB form (r12 is the ModRM escape)
        self.asm.load_qword_idx8(RAX, R12, RDI); // BIGNUM[0]
        self.asm.and_ri(RAX, 1);
        self.asm.je(done);
        self.asm.place(roundup);
        self.asm.mov_ri(RDI, 0);
        self.asm.place(iloop);
        self.asm.load_qword_idx8(RAX, R12, RDI);
        self.asm.add_ri(RAX, 1);
        self.asm.store_qword_idx8(R12, RDI, RAX);
        self.asm.jae(done); // no carry out of this limb → finished
        self.asm.inc_r(RDI);
        self.asm.cmp_ri(RDI, NLIMBS);
        self.asm.jb(iloop);
        self.asm.place(done);
        self.asm.emit(&[0x41, 0x5D, 0x41, 0x5C, 0x5B]); // pop r13; pop r12; pop rbx
        self.asm.emit(&[0xC3]);
    }

    /// `%f` (currently the only conversion routed here). xmm0=value, edi=precision,
    /// esi=flags, edx=width. Mirrors Rust's `{:.P}` byte-for-byte.
    fn emit_fmt_float(&mut self) {
        let bn = self.bignum_global();
        let digbuf = self.alloc_bss(FS_FLOAT_DIGBUF, 1);
        let digend = digbuf + FS_FLOAT_DIGBUF;
        let outbuf = self.outbuf();
        self.asm
            .emit(&[0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]); // push rbx,r12-r15
        self.asm.mov_rr(RBX, RDI); // P
        self.asm.mov_rr(R13, RSI); // flags
        self.asm.mov_rr(R14, RDX); // width
        self.asm.movq_r_from_xmm(R15, 0); // value bits
        // magnitude bits (sign cleared) → keep in r15; sign char → r12.
        let signpos = self.asm.new_label();
        let havesign = self.asm.new_label();
        self.asm.mov_ri64(RAX, 0x7FFF_FFFF_FFFF_FFFF);
        self.asm.mov_rr(RDX, R15);
        self.asm.and_rr(RDX, RAX); // magnitude bits
        self.asm.mov_ri(R12, 0);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(signpos); // ±0 is never negative
        self.asm.mov_rr(RAX, R15);
        self.asm.shr_ri(RAX, 63);
        self.asm.test_rr(RAX, RAX);
        self.asm.je(signpos);
        self.asm.mov_ri(R12, b'-' as i32);
        self.asm.jmp(havesign);
        self.asm.place(signpos);
        self.asm.test_ri(R13, F_PLUS);
        self.asm.je(havesign);
        self.asm.mov_ri(R12, b'+' as i32);
        self.asm.place(havesign);
        // A space flag without plus is handled here (only if no sign yet).
        let nospace = self.asm.new_label();
        self.asm.test_rr(R12, R12);
        self.asm.jne(nospace);
        self.asm.test_ri(R13, F_SPACE);
        self.asm.je(nospace);
        self.asm.mov_ri(R12, b' ' as i32);
        self.asm.place(nospace);
        self.asm.mov_rr(R15, RDX); // r15 = magnitude bits
        // decompose: exp field and fraction.
        let infnan = self.asm.new_label();
        let subnormal = self.asm.new_label();
        let havem = self.asm.new_label();
        self.asm.mov_rr(RDX, R15);
        self.asm.shr_ri(RDX, 52);
        self.asm.and_ri(RDX, 0x7FF); // exp field
        self.asm.mov_ri64(RAX, 0x000F_FFFF_FFFF_FFFF);
        self.asm.mov_rr(RSI, R15);
        self.asm.and_rr(RSI, RAX); // fraction → rsi (becomes mantissa m)
        self.asm.cmp_ri(RDX, 0x7FF);
        self.asm.je(infnan);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(subnormal);
        // normal: m = frac | 2^52, e2 = exp - 1075
        self.asm.mov_ri64(RAX, 0x0010_0000_0000_0000);
        self.asm.or_rr(RSI, RAX);
        self.asm.add_ri(RDX, -1075); // e2 = exp - 1075 (in rdx)
        self.asm.jmp(havem);
        self.asm.place(subnormal);
        self.asm.mov_ri(RDX, -1074); // e2 (m = frac as-is)
        self.asm.place(havem);
        // s = e2 + P; push it across the multiply loop.
        self.asm.mov_rr(RAX, RDX);
        self.asm.add_rr(RAX, RBX);
        self.asm.emit(&[0x50]); // push rax (s)
        // BIGNUM = m (limb 0 = m, rest 0).
        let zloop = self.asm.new_label();
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(RAX, 0);
        self.asm.mov_ri(R9, 0);
        self.asm.place(zloop);
        self.asm.store_qword_idx8(R8, R9, RAX);
        self.asm.inc_r(R9);
        self.asm.cmp_ri(R9, NLIMBS);
        self.asm.jb(zloop);
        self.asm.store_qword_at(R8, RSI); // BIGNUM[0] = m
        // multiply by 5, P times (× 5^P).
        let mulloop = self.asm.new_label();
        let muldone = self.asm.new_label();
        self.asm.mov_rr(R15, RBX); // counter = P
        self.asm.place(mulloop);
        self.asm.test_rr(R15, R15);
        self.asm.je(muldone);
        self.asm.mov_ri(RDI, 5);
        let l = self.helper(Helper::BnMul);
        self.asm.call(l);
        self.asm.dec_r(R15);
        self.asm.jmp(mulloop);
        self.asm.place(muldone);
        // × 2^s : shift left by s (or right, rounding, when s < 0).
        let shrpath = self.asm.new_label();
        let shifted = self.asm.new_label();
        self.asm.emit(&[0x58]); // pop rax (s)
        self.asm.test_rr(RAX, RAX);
        self.asm.js(shrpath);
        self.asm.mov_rr(RDI, RAX);
        let l = self.helper(Helper::BnShl);
        self.asm.call(l);
        self.asm.jmp(shifted);
        self.asm.place(shrpath);
        self.asm.neg_r(RAX);
        self.asm.mov_rr(RDI, RAX);
        let l = self.helper(Helper::BnShr);
        self.asm.call(l);
        self.asm.place(shifted);
        // extract decimal digits of J into DIGBUF (written downward from the end).
        let dloop = self.asm.new_label();
        let zchk = self.asm.new_label();
        let notzero = self.asm.new_label();
        let extracted = self.asm.new_label();
        self.asm.lea_global(R15, digend); // digit cursor (grows down)
        self.asm.place(dloop);
        let l = self.helper(Helper::BnDiv10);
        self.asm.call(l); // rax = next digit
        self.asm.add_ri(RAX, b'0' as i32);
        self.asm.dec_r(R15);
        self.asm.store_byte_at(R15, RAX);
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(R9, 0);
        self.asm.place(zchk);
        self.asm.load_qword_idx8(RAX, R8, R9);
        self.asm.test_rr(RAX, RAX);
        self.asm.jne(notzero);
        self.asm.inc_r(R9);
        self.asm.cmp_ri(R9, NLIMBS);
        self.asm.jb(zchk);
        self.asm.jmp(extracted);
        self.asm.place(notzero);
        self.asm.jmp(dloop);
        self.asm.place(extracted);
        // ndig = digend - r15 → r11
        self.asm.lea_global(RAX, digend);
        self.asm.sub_rr(RAX, R15);
        self.asm.mov_rr(R11, RAX); // ndig
        // bodylen → r10: P==0 ⇒ ndig; ndig>P ⇒ ndig+1; else ⇒ P+2.
        let pnz = self.asm.new_label();
        let small = self.asm.new_label();
        let haveblen = self.asm.new_label();
        self.asm.test_rr(RBX, RBX);
        self.asm.jne(pnz);
        self.asm.mov_rr(R10, R11);
        self.asm.jmp(haveblen);
        self.asm.place(pnz);
        self.asm.cmp_reg_reg(R11, RBX);
        self.asm.jbe(small);
        self.asm.mov_rr(R10, R11);
        self.asm.inc_r(R10);
        self.asm.jmp(haveblen);
        self.asm.place(small);
        self.asm.mov_rr(R10, RBX);
        self.asm.add_ri(R10, 2);
        self.asm.place(haveblen);
        // pad = max(0, width - (bodylen + signlen)) → rdx
        let nosl = self.asm.new_label();
        let padok = self.asm.new_label();
        self.asm.mov_rr(RAX, R10);
        self.asm.test_rr(R12, R12);
        self.asm.je(nosl);
        self.asm.inc_r(RAX);
        self.asm.place(nosl);
        self.asm.mov_rr(RDX, R14);
        self.asm.sub_rr(RDX, RAX);
        self.asm.jns(padok);
        self.asm.mov_ri(RDX, 0);
        self.asm.place(padok);
        // assemble the field into OUTBUF (cursor r8); no calls until out_write.
        self.asm.lea_global(R8, outbuf);
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let fielddone = self.asm.new_label();
        self.asm.test_ri(R13, F_MINUS);
        self.asm.jne(do_minus);
        self.asm.test_ri(R13, F_ZERO);
        self.asm.je(do_right);
        self.float_emit_sign();
        self.float_emit_pad(b'0');
        self.float_emit_body(digbuf);
        self.asm.jmp(fielddone);
        self.asm.place(do_right);
        self.float_emit_pad(b' ');
        self.float_emit_sign();
        self.float_emit_body(digbuf);
        self.asm.jmp(fielddone);
        self.asm.place(do_minus);
        self.float_emit_sign();
        self.float_emit_body(digbuf);
        self.float_emit_pad(b' ');
        self.asm.place(fielddone);
        // out_write(OUTBUF, cursor - OUTBUF)
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, R8);
        self.asm.sub_rr(RDX, RSI);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
        let epilogue = self.asm.new_label();
        self.asm.jmp(epilogue);
        // inf / NaN: emit the sign then "inf"/"NaN" (Rust's spelling), unpadded.
        // rsi still holds the fraction field (nonzero ⇒ NaN), r12 the sign.
        self.asm.place(infnan);
        self.asm.lea_global(R8, outbuf);
        self.float_emit_sign();
        let is_nan = self.asm.new_label();
        let wrote = self.asm.new_label();
        self.asm.test_rr(RSI, RSI); // rsi = fraction; nonzero ⇒ NaN
        self.asm.jne(is_nan);
        self.asm.store_byte_imm_at(R8, b'i');
        self.asm.inc_r(R8);
        self.asm.store_byte_imm_at(R8, b'n');
        self.asm.inc_r(R8);
        self.asm.store_byte_imm_at(R8, b'f');
        self.asm.inc_r(R8);
        self.asm.jmp(wrote);
        self.asm.place(is_nan);
        self.asm.store_byte_imm_at(R8, b'N');
        self.asm.inc_r(R8);
        self.asm.store_byte_imm_at(R8, b'a');
        self.asm.inc_r(R8);
        self.asm.store_byte_imm_at(R8, b'N');
        self.asm.inc_r(R8);
        self.asm.place(wrote);
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, R8);
        self.asm.sub_rr(RDX, RSI);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
        self.asm.place(epilogue);
        self.asm
            .emit(&[0x41, 0x5F, 0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C, 0x5B]); // pop r15..rbx
        self.asm.emit(&[0xC3]);
    }

    /// Append the sign byte (r12, 0 ⇒ none) to the OUTBUF cursor r8.
    fn float_emit_sign(&mut self) {
        let skip = self.asm.new_label();
        self.asm.test_rr(R12, R12);
        self.asm.je(skip);
        self.asm.store_byte_at(R8, R12);
        self.asm.inc_r(R8);
        self.asm.place(skip);
    }
    /// Append rdx copies of byte `fill` to the OUTBUF cursor r8.
    fn float_emit_pad(&mut self, fill: u8) {
        let top = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.place(top);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(done);
        self.asm.store_byte_imm_at(R8, fill);
        self.asm.inc_r(R8);
        self.asm.dec_r(RDX);
        self.asm.jmp(top);
        self.asm.place(done);
    }
    /// Append the `%f` body (digits in DIGBUF at r15, count r11, precision rbx) to
    /// the OUTBUF cursor r8: `int.frac`, or `0.0…digits`, or the bare integer (P=0).
    fn float_emit_body(&mut self, _digbuf: i32) {
        let pnz = self.asm.new_label();
        let small = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.test_rr(RBX, RBX);
        self.asm.jne(pnz);
        // P == 0: copy all ndig integer digits
        self.asm.mov_rr(R9, R15);
        self.asm.mov_rr(RCX, R11);
        self.float_copy_digits();
        self.asm.jmp(done);
        self.asm.place(pnz);
        self.asm.cmp_reg_reg(R11, RBX);
        self.asm.jbe(small);
        // ndig > P: (ndig-P) integer digits, '.', then P fractional digits
        self.asm.mov_rr(R9, R15);
        self.asm.mov_rr(RCX, R11);
        self.asm.sub_rr(RCX, RBX);
        self.float_copy_digits();
        self.asm.store_byte_imm_at(R8, b'.');
        self.asm.inc_r(R8);
        self.asm.mov_rr(RCX, RBX);
        self.float_copy_digits(); // r9 already advanced to the fractional digits
        self.asm.jmp(done);
        self.asm.place(small);
        // ndig <= P: "0", '.', (P-ndig) zeros, then ndig digits
        self.asm.store_byte_imm_at(R8, b'0');
        self.asm.inc_r(R8);
        self.asm.store_byte_imm_at(R8, b'.');
        self.asm.inc_r(R8);
        let zloop = self.asm.new_label();
        let zdone = self.asm.new_label();
        self.asm.mov_rr(RCX, RBX);
        self.asm.sub_rr(RCX, R11); // P - ndig leading zeros
        self.asm.place(zloop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(zdone);
        self.asm.store_byte_imm_at(R8, b'0');
        self.asm.inc_r(R8);
        self.asm.dec_r(RCX);
        self.asm.jmp(zloop);
        self.asm.place(zdone);
        self.asm.mov_rr(R9, R15);
        self.asm.mov_rr(RCX, R11);
        self.float_copy_digits();
        self.asm.place(done);
    }
    /// Copy rcx digit bytes from r9 to the OUTBUF cursor r8 (advancing both).
    fn float_copy_digits(&mut self) {
        self.asm.mov_rr(RSI, R9);
        self.asm.mov_rr(RDI, R8);
        self.asm.rep_movsb();
        self.asm.mov_rr(R8, RDI);
        self.asm.mov_rr(R9, RSI);
    }

    /// `%e`/`%g`: format from the value's exact decimal digit string (since a
    /// double `m·2^e` is the dyadic rational `Dint·10^pe`, `Dint` integer), rounded
    /// to N significant figures. Mirrors `fmt::render_exp`/`render_g` byte-for-byte.
    fn emit_fmt_float_eg(&mut self) {
        let bn = self.bignum_global();
        let egdig = self.alloc_bss(1024, 1);
        let egend = egdig + 1024;
        let sigbuf = self.alloc_bss(FS_SIGBUF, 1);
        let bodybuf = self.alloc_bss(1024, 1);
        let outbuf = self.outbuf();
        const CONV: i8 = 0;
        const PE: i8 = 8;
        const XOFF: i8 = 16;
        const NSIG: i8 = 24;
        // prologue + 64-byte frame
        self.asm
            .emit(&[0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]);
        self.asm.sub_rsp(64);
        self.asm.mov_rr(RBX, RDI); // P
        self.asm.mov_rr(R13, RSI); // flags
        self.asm.mov_rr(R14, RDX); // width
        self.asm.store_rsp(CONV, RCX); // conv
        // sign (r12) + magnitude bits (r15)
        let sgpos = self.asm.new_label();
        let strysp = self.asm.new_label();
        let shavesign = self.asm.new_label();
        self.asm.movq_r_from_xmm(R15, 0);
        self.asm.mov_ri64(RAX, 0x7FFF_FFFF_FFFF_FFFF);
        self.asm.mov_rr(RDX, R15);
        self.asm.and_rr(RDX, RAX);
        self.asm.mov_ri(R12, 0);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(sgpos);
        self.asm.mov_rr(RAX, R15);
        self.asm.shr_ri(RAX, 63);
        self.asm.test_rr(RAX, RAX);
        self.asm.je(sgpos);
        self.asm.mov_ri(R12, b'-' as i32);
        self.asm.jmp(shavesign);
        self.asm.place(sgpos);
        self.asm.test_ri(R13, F_PLUS);
        self.asm.je(strysp);
        self.asm.mov_ri(R12, b'+' as i32);
        self.asm.jmp(shavesign);
        self.asm.place(strysp);
        self.asm.test_ri(R13, F_SPACE);
        self.asm.je(shavesign);
        self.asm.mov_ri(R12, b' ' as i32);
        self.asm.place(shavesign);
        self.asm.mov_rr(R15, RDX); // magnitude bits
        // decompose → exp(rdx), frac(rsi)
        let eg_infnan = self.asm.new_label();
        let eg_zero = self.asm.new_label();
        let eg_subn = self.asm.new_label();
        let eg_havem = self.asm.new_label();
        self.asm.mov_rr(RDX, R15);
        self.asm.shr_ri(RDX, 52);
        self.asm.and_ri(RDX, 0x7FF);
        self.asm.mov_ri64(RAX, 0x000F_FFFF_FFFF_FFFF);
        self.asm.mov_rr(RSI, R15);
        self.asm.and_rr(RSI, RAX); // frac
        self.asm.cmp_ri(RDX, 0x7FF);
        self.asm.je(eg_infnan);
        self.asm.test_rr(R15, R15);
        self.asm.je(eg_zero);
        self.asm.test_rr(RDX, RDX);
        self.asm.je(eg_subn);
        self.asm.mov_ri64(RAX, 0x0010_0000_0000_0000);
        self.asm.or_rr(RSI, RAX);
        self.asm.add_ri(RDX, -1075);
        self.asm.jmp(eg_havem);
        self.asm.place(eg_subn);
        self.asm.mov_ri(RDX, -1074);
        self.asm.place(eg_havem);
        // m=rsi, e2=rdx. BIGNUM = m.
        let ezloop = self.asm.new_label();
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(RAX, 0);
        self.asm.mov_ri(R9, 0);
        self.asm.place(ezloop);
        self.asm.store_qword_idx8(R8, R9, RAX);
        self.asm.inc_r(R9);
        self.asm.cmp_ri(R9, NLIMBS);
        self.asm.jb(ezloop);
        self.asm.store_qword_at(R8, RSI); // BIGNUM[0] = m
        // pe = min(e2,0); build Dint = m·2^e2 (e2≥0) or m·5^(−e2) (e2<0).
        let eg_neg = self.asm.new_label();
        let eg_mulloop = self.asm.new_label();
        let eg_built = self.asm.new_label();
        self.asm.test_rr(RDX, RDX);
        self.asm.js(eg_neg);
        self.asm.store_rsp_imm(PE, 0);
        self.asm.mov_rr(RDI, RDX);
        let l = self.helper(Helper::BnShl);
        self.asm.call(l);
        self.asm.jmp(eg_built);
        self.asm.place(eg_neg);
        self.asm.store_rsp(PE, RDX); // pe = e2
        self.asm.mov_rr(R15, RDX);
        self.asm.neg_r(R15); // count = -e2
        self.asm.place(eg_mulloop);
        self.asm.test_rr(R15, R15);
        self.asm.je(eg_built);
        self.asm.mov_ri(RDI, 5);
        let l = self.helper(Helper::BnMul);
        self.asm.call(l);
        self.asm.dec_r(R15);
        self.asm.jmp(eg_mulloop);
        self.asm.place(eg_built);
        // extract all digits of Dint into EGDIG (downward); r15 = MSB ptr.
        let eg_dloop = self.asm.new_label();
        let eg_zchk = self.asm.new_label();
        let eg_notz = self.asm.new_label();
        let eg_done = self.asm.new_label();
        self.asm.lea_global(R15, egend);
        self.asm.place(eg_dloop);
        let l = self.helper(Helper::BnDiv10);
        self.asm.call(l);
        self.asm.add_ri(RAX, b'0' as i32);
        self.asm.dec_r(R15);
        self.asm.store_byte_at(R15, RAX);
        self.asm.lea_global(R8, bn);
        self.asm.mov_ri(R9, 0);
        self.asm.place(eg_zchk);
        self.asm.load_qword_idx8(RAX, R8, R9);
        self.asm.test_rr(RAX, RAX);
        self.asm.jne(eg_notz);
        self.asm.inc_r(R9);
        self.asm.cmp_ri(R9, NLIMBS);
        self.asm.jb(eg_zchk);
        self.asm.jmp(eg_done);
        self.asm.place(eg_notz);
        self.asm.jmp(eg_dloop);
        self.asm.place(eg_done);
        // ndig (rcx), X = ndig - 1 + pe → [XOFF]
        self.asm.lea_global(RAX, egend);
        self.asm.sub_rr(RAX, R15);
        self.asm.mov_rr(RCX, RAX); // ndig
        self.asm.load_rsp(RDX, PE);
        self.asm.add_rr(RAX, RDX);
        self.asm.add_ri(RAX, -1);
        self.asm.store_rsp(XOFF, RAX); // X
        // nsig: e → P+1; g → max(P,1). conv kind in low 2 bits.
        let eg_isg = self.asm.new_label();
        let eg_havensig = self.asm.new_label();
        self.asm.load_rsp(RAX, CONV);
        self.asm.and_ri(RAX, 3);
        self.asm.cmp_ri(RAX, 2);
        self.asm.je(eg_isg);
        self.asm.mov_rr(RAX, RBX);
        self.asm.add_ri(RAX, 1); // P+1
        self.asm.jmp(eg_havensig);
        self.asm.place(eg_isg);
        self.asm.mov_rr(RAX, RBX); // max(P,1)
        self.asm.test_rr(RAX, RAX);
        self.asm.jne(eg_havensig);
        self.asm.mov_ri(RAX, 1);
        self.asm.place(eg_havensig);
        self.asm.store_rsp(NSIG, RAX);
        // round EGDIG[r15..] (ndig=rcx) to nsig=rax significant figures.
        // real = min(ndig, nsig); if ndig > nsig, round (may bump X).
        let eg_noround = self.asm.new_label();
        let eg_haveround = self.asm.new_label();
        self.asm.cmp_reg_reg(RCX, RAX); // ndig vs nsig
        self.asm.jbe(eg_noround);
        self.asm.store_rsp(32, RCX); // stash ndig for eg_round
        self.eg_round(); // rounds in place; updates [XOFF]; leaves real=nsig
        self.asm.load_rsp(RAX, NSIG);
        self.asm.mov_rr(R11, RAX); // real = nsig
        self.asm.jmp(eg_haveround);
        self.asm.place(eg_noround);
        self.asm.mov_rr(R11, RCX); // real = ndig
        self.asm.place(eg_haveround);
        // copy `real` digits (r15) + pad with '0' to nsig into SIGBUF.
        let eg_cploop = self.asm.new_label();
        let eg_cpz = self.asm.new_label();
        let eg_cpput = self.asm.new_label();
        let eg_cpdone = self.asm.new_label();
        self.asm.lea_global(R8, sigbuf); // dst
        self.asm.mov_rr(R9, R15); // src
        self.asm.load_rsp(R10, NSIG);
        self.asm.mov_ri(RDI, 0); // i
        self.asm.place(eg_cploop);
        self.asm.cmp_reg_reg(RDI, R10);
        self.asm.jae(eg_cpdone);
        self.asm.cmp_reg_reg(RDI, R11); // i < real?
        self.asm.jae(eg_cpz);
        self.asm.load_byte_zx(RAX, R9);
        self.asm.inc_r(R9);
        self.asm.jmp(eg_cpput);
        self.asm.place(eg_cpz);
        self.asm.mov_ri(RAX, b'0' as i32);
        self.asm.place(eg_cpput);
        self.asm.store_byte_at(R8, RAX);
        self.asm.inc_r(R8);
        self.asm.inc_r(RDI);
        self.asm.jmp(eg_cploop);
        self.asm.place(eg_cpdone);
        // SIGBUF[0..nsig] now holds the rounded significant digits.
        // Decide formatting style and build BODYBUF; r15 = SIGBUF base.
        self.asm.lea_global(R15, sigbuf);
        self.eg_format(sigbuf, bodybuf);
        // field render: sign + pad + BODYBUF → OUTBUF → out_write.
        // (rdi = end of BODYBUF after eg_format; bodylen = rdi - bodybuf)
        self.eg_field(bodybuf, outbuf);
        let epilogue = self.asm.new_label();
        self.asm.jmp(epilogue);
        // value == 0: digits are all '0'; format with X = 0.
        self.asm.place(eg_zero);
        self.asm.store_rsp_imm(XOFF, 0);
        let eg_isg0 = self.asm.new_label();
        let eg_hns0 = self.asm.new_label();
        self.asm.load_rsp(RAX, CONV);
        self.asm.and_ri(RAX, 3);
        self.asm.cmp_ri(RAX, 2);
        self.asm.je(eg_isg0);
        self.asm.mov_rr(RAX, RBX);
        self.asm.add_ri(RAX, 1);
        self.asm.jmp(eg_hns0);
        self.asm.place(eg_isg0);
        self.asm.mov_rr(RAX, RBX);
        self.asm.test_rr(RAX, RAX);
        self.asm.jne(eg_hns0);
        self.asm.mov_ri(RAX, 1);
        self.asm.place(eg_hns0);
        self.asm.store_rsp(NSIG, RAX);
        // SIGBUF = nsig zeros
        let eg_z2 = self.asm.new_label();
        let eg_z2d = self.asm.new_label();
        self.asm.lea_global(R8, sigbuf);
        self.asm.mov_ri(RCX, 0);
        self.asm.place(eg_z2);
        self.asm.cmp_reg_reg(RCX, RAX);
        self.asm.jae(eg_z2d);
        self.asm.store_byte_imm_at(R8, b'0');
        self.asm.inc_r(R8);
        self.asm.inc_r(RCX);
        self.asm.jmp(eg_z2);
        self.asm.place(eg_z2d);
        self.asm.lea_global(R15, sigbuf);
        self.eg_format(sigbuf, bodybuf);
        self.eg_field(bodybuf, outbuf);
        self.asm.jmp(epilogue);
        // inf / NaN: sign then "inf"/"NaN" (rsi = fraction, nonzero ⇒ NaN).
        self.asm.place(eg_infnan);
        self.asm.lea_global(R8, outbuf);
        self.float_emit_sign();
        let eg_nan = self.asm.new_label();
        let eg_wrote = self.asm.new_label();
        self.asm.test_rr(RSI, RSI);
        self.asm.jne(eg_nan);
        for c in [b'i', b'n', b'f'] {
            self.asm.store_byte_imm_at(R8, c);
            self.asm.inc_r(R8);
        }
        self.asm.jmp(eg_wrote);
        self.asm.place(eg_nan);
        for c in [b'N', b'a', b'N'] {
            self.asm.store_byte_imm_at(R8, c);
            self.asm.inc_r(R8);
        }
        self.asm.place(eg_wrote);
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, R8);
        self.asm.sub_rr(RDX, RSI);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
        self.asm.place(epilogue);
        self.asm.add_rsp(64);
        self.asm
            .emit(&[0x41, 0x5F, 0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C, 0x5B]);
        self.asm.emit(&[0xC3]);
    }

    /// Round the digit string at r15 (`ndig` digits at `[rsp+32]`, ndig > nsig) to
    /// nsig (`[rsp+24]`) significant figures, in place. Round-half-to-even on the
    /// trailing digits; a carry overflow ("99..9"→"10..0") bumps X (`[rsp+16]`).
    fn eg_round(&mut self) {
        let sloop = self.asm.new_label();
        let nstick = self.asm.new_label();
        let sdone = self.asm.new_label();
        let roundup = self.asm.new_label();
        let noup = self.asm.new_label();
        let iloop = self.asm.new_label();
        let carry = self.asm.new_label();
        let overflow = self.asm.new_label();
        let done = self.asm.new_label();
        self.asm.load_rsp(R8, 24); // nsig
        // round digit = r15[nsig]  → rcx
        self.asm.mov_rr(RAX, R15);
        self.asm.add_rr(RAX, R8);
        self.asm.load_byte_zx(RCX, RAX);
        // sticky: any digit in (nsig, ndig) ≠ '0'  → r9
        self.asm.mov_ri(R9, 0);
        self.asm.mov_rr(RDX, RAX);
        self.asm.inc_r(RDX); // ptr to r15[nsig+1]
        self.asm.load_rsp(R10, 32); // ndig
        self.asm.mov_rr(RAX, R15);
        self.asm.add_rr(RAX, R10); // end = r15 + ndig
        self.asm.place(sloop);
        self.asm.cmp_reg_reg(RDX, RAX);
        self.asm.jae(sdone);
        self.asm.load_byte_zx(R11, RDX);
        self.asm.cmp_ri(R11, b'0' as i32);
        self.asm.je(nstick);
        self.asm.mov_ri(R9, 1);
        self.asm.jmp(sdone);
        self.asm.place(nstick);
        self.asm.inc_r(RDX);
        self.asm.jmp(sloop);
        self.asm.place(sdone);
        // round up if digit>'5', or (=='5' and (sticky or last-kept odd))
        self.asm.cmp_ri(RCX, b'5' as i32);
        self.asm.jg(roundup);
        self.asm.jl(noup);
        self.asm.test_rr(R9, R9);
        self.asm.jne(roundup);
        self.asm.mov_rr(RAX, R15);
        self.asm.add_rr(RAX, R8);
        self.asm.add_ri(RAX, -1); // &r15[nsig-1]
        self.asm.load_byte_zx(RDX, RAX);
        self.asm.and_ri(RDX, 1); // ASCII parity = digit parity
        self.asm.je(noup);
        self.asm.place(roundup);
        // increment the nsig-digit number from the last digit
        self.asm.mov_rr(RDX, R8);
        self.asm.add_ri(RDX, -1); // index = nsig-1
        self.asm.place(iloop);
        self.asm.mov_rr(RAX, R15);
        self.asm.add_rr(RAX, RDX); // &digit[idx]
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_ri(RCX, b'9' as i32);
        self.asm.je(carry);
        self.asm.add_ri(RCX, 1);
        self.asm.store_byte_at(RAX, RCX);
        self.asm.jmp(done);
        self.asm.place(carry);
        self.asm.store_byte_imm_at(RAX, b'0');
        self.asm.dec_r(RDX);
        self.asm.js(overflow);
        self.asm.jmp(iloop);
        self.asm.place(overflow);
        // all '9' → "1" followed by zeros (already set); X += 1
        self.asm.store_byte_imm_at(R15, b'1');
        self.asm.load_rsp(RDX, 16);
        self.asm.add_ri(RDX, 1);
        self.asm.store_rsp(16, RDX);
        self.asm.place(noup);
        self.asm.place(done);
    }

    /// Build the `%e`/`%g` numeric body into BODYBUF from SIGBUF (r15, nsig digits)
    /// and X. Leaves the BODYBUF end in rdi. `%g` trims trailing zeros (unless `#`).
    fn eg_format(&mut self, _sigbuf: i32, bodybuf: i32) {
        // dispatch on conv kind (g uses %f-style or %e-style; e is always %e-style)
        let kind_g = self.asm.new_label();
        let use_e = self.asm.new_label();
        let trim = self.asm.new_label();
        let no_trim = self.asm.new_label();
        self.asm.lea_global(RDI, bodybuf); // body cursor
        self.asm.load_rsp(RAX, 0); // conv
        self.asm.and_ri(RAX, 3);
        self.asm.cmp_ri(RAX, 2);
        self.asm.je(kind_g);
        // %e: precision = P (rbx), nsig = P+1
        self.eg_body_sci(RBX);
        self.asm.jmp(no_trim);
        self.asm.place(kind_g);
        // %g: p = nsig; X decides %f-style (−4 ≤ X < p) vs %e-style.
        self.asm.load_rsp(RAX, 16); // X
        self.asm.cmp_ri(RAX, -4);
        self.asm.jl(use_e);
        self.asm.load_rsp(RCX, 24); // p (= nsig)
        self.asm.cmp_reg_reg(RAX, RCX);
        self.asm.jge(use_e);
        // %f-style: fixed notation from the digits
        self.eg_body_fixed();
        self.asm.jmp(trim);
        self.asm.place(use_e);
        // %e-style with precision p-1
        self.asm.load_rsp(RAX, 24);
        self.asm.add_ri(RAX, -1); // p-1
        self.asm.mov_rr(R8, RAX);
        self.eg_body_sci(R8);
        self.asm.place(trim);
        // trim trailing zeros (unless the # flag)
        self.asm.test_ri(R13, F_HASH);
        self.asm.jne(no_trim);
        self.eg_trim(bodybuf);
        self.asm.place(no_trim);
    }

    /// `%e`-style body: `d`, then `.fff` (precision `prec_reg` digits), then
    /// `e`/`E`, sign, and the ≥2-digit exponent. Reads SIGBUF (r15), X (`[rsp+16]`).
    fn eg_body_sci(&mut self, prec_reg: u8) {
        let pr = if prec_reg == RBX { RBX } else { prec_reg };
        // first digit
        self.asm.load_byte_zx(RAX, R15);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        // if prec > 0: '.', then prec fractional digits SIGBUF[1..1+prec]
        let nofrac = self.asm.new_label();
        let floop = self.asm.new_label();
        let fdone = self.asm.new_label();
        self.asm.mov_rr(RCX, pr);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(nofrac);
        self.asm.store_byte_imm_at(RDI, b'.');
        self.asm.inc_r(RDI);
        self.asm.mov_rr(R8, R15);
        self.asm.inc_r(R8); // &SIGBUF[1]
        self.asm.place(floop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(fdone);
        self.asm.load_byte_zx(RAX, R8);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.inc_r(R8);
        self.asm.dec_r(RCX);
        self.asm.jmp(floop);
        self.asm.place(fdone);
        self.asm.place(nofrac);
        // 'e' / 'E'
        let lower = self.asm.new_label();
        let wrote_e = self.asm.new_label();
        self.asm.load_rsp(RAX, 0); // conv
        self.asm.test_ri(RAX, 4); // upper bit
        self.asm.je(lower);
        self.asm.store_byte_imm_at(RDI, b'E');
        self.asm.jmp(wrote_e);
        self.asm.place(lower);
        self.asm.store_byte_imm_at(RDI, b'e');
        self.asm.place(wrote_e);
        self.asm.inc_r(RDI);
        // exponent sign + |X| (≥ 2 digits)
        let xneg = self.asm.new_label();
        let xpos = self.asm.new_label();
        self.asm.load_rsp(RAX, 16); // X
        self.asm.test_rr(RAX, RAX);
        self.asm.jns(xpos);
        self.asm.store_byte_imm_at(RDI, b'-');
        self.asm.inc_r(RDI);
        self.asm.neg_r(RAX);
        self.asm.jmp(xneg);
        self.asm.place(xpos);
        self.asm.store_byte_imm_at(RDI, b'+');
        self.asm.inc_r(RDI);
        self.asm.place(xneg);
        // rax = |X|; emit hundreds (if any), then tens, ones (min 2 digits)
        let nohund = self.asm.new_label();
        self.asm.mov_ri(RCX, 100);
        self.asm.mov_ri(RDX, 0);
        self.asm.div_r(RCX); // rax = X/100, rdx = X%100
        self.asm.test_rr(RAX, RAX);
        self.asm.je(nohund);
        self.asm.add_ri(RAX, b'0' as i32);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.place(nohund);
        // rdx = X%100; tens then ones
        self.asm.mov_rr(RAX, RDX);
        self.asm.mov_ri(RCX, 10);
        self.asm.mov_ri(RDX, 0);
        self.asm.div_r(RCX); // rax = tens, rdx = ones
        self.asm.add_ri(RAX, b'0' as i32);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.add_ri(RDX, b'0' as i32);
        self.asm.store_byte_at(RDI, RDX);
        self.asm.inc_r(RDI);
    }

    /// `%g` `%f`-style body: fixed notation of the SIGBUF digits placed by X.
    fn eg_body_fixed(&mut self) {
        // X (r9), nsig (r10), SIGBUF base (r15), cursor rdi.
        self.asm.load_rsp(R9, 16); // X
        self.asm.load_rsp(R10, 24); // nsig (= p)
        let xneg = self.asm.new_label();
        // if X >= 0: int = SIGBUF[0..X+1], frac = SIGBUF[X+1..nsig]
        self.asm.test_rr(R9, R9);
        self.asm.js(xneg);
        // integer digits: count = X+1
        let iloop = self.asm.new_label();
        let idone = self.asm.new_label();
        self.asm.mov_rr(R8, R15); // src
        self.asm.mov_rr(RCX, R9);
        self.asm.add_ri(RCX, 1); // X+1
        self.asm.place(iloop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(idone);
        self.asm.load_byte_zx(RAX, R8);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.inc_r(R8);
        self.asm.dec_r(RCX);
        self.asm.jmp(iloop);
        self.asm.place(idone);
        // frac: remaining nsig-(X+1) digits, if any
        let nofrac = self.asm.new_label();
        let floop = self.asm.new_label();
        let fdone = self.asm.new_label();
        self.asm.mov_rr(RCX, R10);
        self.asm.sub_rr(RCX, R9);
        self.asm.add_ri(RCX, -1); // nsig - X - 1
        self.asm.test_rr(RCX, RCX);
        self.asm.jle(nofrac);
        self.asm.store_byte_imm_at(RDI, b'.');
        self.asm.inc_r(RDI);
        self.asm.place(floop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(fdone);
        self.asm.load_byte_zx(RAX, R8);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.inc_r(R8);
        self.asm.dec_r(RCX);
        self.asm.jmp(floop);
        self.asm.place(fdone);
        self.asm.place(nofrac);
        let done = self.asm.new_label();
        self.asm.jmp(done);
        // X < 0: "0." then (−X−1) zeros then all nsig digits
        self.asm.place(xneg);
        self.asm.store_byte_imm_at(RDI, b'0');
        self.asm.inc_r(RDI);
        self.asm.store_byte_imm_at(RDI, b'.');
        self.asm.inc_r(RDI);
        let zloop = self.asm.new_label();
        let zdone = self.asm.new_label();
        self.asm.mov_rr(RCX, R9);
        self.asm.neg_r(RCX);
        self.asm.add_ri(RCX, -1); // -X-1 leading zeros
        self.asm.place(zloop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(zdone);
        self.asm.store_byte_imm_at(RDI, b'0');
        self.asm.inc_r(RDI);
        self.asm.dec_r(RCX);
        self.asm.jmp(zloop);
        self.asm.place(zdone);
        let dloop = self.asm.new_label();
        let ddone = self.asm.new_label();
        self.asm.mov_rr(R8, R15);
        self.asm.mov_rr(RCX, R10);
        self.asm.place(dloop);
        self.asm.test_rr(RCX, RCX);
        self.asm.je(ddone);
        self.asm.load_byte_zx(RAX, R8);
        self.asm.store_byte_at(RDI, RAX);
        self.asm.inc_r(RDI);
        self.asm.inc_r(R8);
        self.asm.dec_r(RCX);
        self.asm.jmp(dloop);
        self.asm.place(ddone);
        self.asm.place(done);
    }

    /// Trim trailing zeros from BODYBUF's mantissa (and a bare `.`), keeping any
    /// exponent suffix. The body is `[bodybuf .. rdi)`; rdi is updated.
    fn eg_trim(&mut self, bodybuf: i32) {
        // find 'e'/'E' (split point) → r8 = mantissa end; r9 = exponent start/len.
        let scan = self.asm.new_label();
        let found_e = self.asm.new_label();
        let no_e = self.asm.new_label();
        let have_split = self.asm.new_label();
        self.asm.lea_global(RAX, bodybuf); // p
        self.asm.place(scan);
        self.asm.cmp_reg_reg(RAX, RDI);
        self.asm.jae(no_e);
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_ri(RCX, b'e' as i32);
        self.asm.je(found_e);
        self.asm.cmp_ri(RCX, b'E' as i32);
        self.asm.je(found_e);
        self.asm.inc_r(RAX);
        self.asm.jmp(scan);
        self.asm.place(no_e);
        self.asm.mov_rr(R8, RDI); // mantissa end = body end
        self.asm.mov_ri(R9, 0); // no exponent
        self.asm.jmp(have_split);
        self.asm.place(found_e);
        self.asm.mov_rr(R8, RAX); // mantissa end = 'e' position
        self.asm.mov_rr(R9, RDI);
        self.asm.sub_rr(R9, RAX); // exponent length
        self.asm.place(have_split);
        // only trim if the mantissa contains '.'
        let mscan = self.asm.new_label();
        let has_dot = self.asm.new_label();
        let nodot = self.asm.new_label();
        self.asm.lea_global(RAX, bodybuf);
        self.asm.place(mscan);
        self.asm.cmp_reg_reg(RAX, R8);
        self.asm.jae(nodot);
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_ri(RCX, b'.' as i32);
        self.asm.je(has_dot);
        self.asm.inc_r(RAX);
        self.asm.jmp(mscan);
        self.asm.place(has_dot);
        // strip trailing '0' then a trailing '.' from [bodybuf .. r8)
        let tloop = self.asm.new_label();
        let tdone = self.asm.new_label();
        self.asm.place(tloop);
        self.asm.mov_rr(RAX, R8);
        self.asm.add_ri(RAX, -1); // last mantissa char
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_ri(RCX, b'0' as i32);
        self.asm.jne(tdone);
        self.asm.dec_r(R8);
        self.asm.jmp(tloop);
        self.asm.place(tdone);
        // drop a trailing '.'
        let nodot2 = self.asm.new_label();
        self.asm.mov_rr(RAX, R8);
        self.asm.add_ri(RAX, -1);
        self.asm.load_byte_zx(RCX, RAX);
        self.asm.cmp_ri(RCX, b'.' as i32);
        self.asm.jne(nodot2);
        self.asm.dec_r(R8);
        self.asm.place(nodot2);
        // move the exponent suffix down to r8 (if any), set rdi = new end
        let eloop = self.asm.new_label();
        let edone = self.asm.new_label();
        self.asm.mov_rr(RSI, RDI);
        self.asm.sub_rr(RSI, R9); // exponent source start = old end - explen
        self.asm.place(eloop);
        self.asm.test_rr(R9, R9);
        self.asm.je(edone);
        self.asm.load_byte_zx(RAX, RSI);
        self.asm.store_byte_at(R8, RAX);
        self.asm.inc_r(R8);
        self.asm.inc_r(RSI);
        self.asm.dec_r(R9);
        self.asm.jmp(eloop);
        self.asm.place(edone);
        self.asm.mov_rr(RDI, R8); // new body end
        self.asm.place(nodot);
    }

    /// Render the assembled body (`[bodybuf .. rdi)`) as a field: sign + width pad
    /// (precision is None for floats, so a `0` flag zero-pads) → OUTBUF → out_write.
    fn eg_field(&mut self, bodybuf: i32, outbuf: i32) {
        // bodylen = rdi - bodybuf → r11
        self.asm.lea_global(RAX, bodybuf);
        self.asm.mov_rr(R11, RDI);
        self.asm.sub_rr(R11, RAX); // bodylen
        // pad = max(0, width - (bodylen + signlen)) → rdx
        let nosl = self.asm.new_label();
        let padok = self.asm.new_label();
        self.asm.mov_rr(RAX, R11);
        self.asm.test_rr(R12, R12);
        self.asm.je(nosl);
        self.asm.inc_r(RAX);
        self.asm.place(nosl);
        self.asm.mov_rr(RDX, R14);
        self.asm.sub_rr(RDX, RAX);
        self.asm.jns(padok);
        self.asm.mov_ri(RDX, 0);
        self.asm.place(padok);
        // assemble: justify by flags (minus / zero / right)
        self.asm.lea_global(R8, outbuf); // cursor
        let do_minus = self.asm.new_label();
        let do_right = self.asm.new_label();
        let fielddone = self.asm.new_label();
        self.asm.test_ri(R13, F_MINUS);
        self.asm.jne(do_minus);
        self.asm.test_ri(R13, F_ZERO);
        self.asm.je(do_right);
        self.float_emit_sign();
        self.float_emit_pad(b'0');
        self.eg_copy_body(bodybuf);
        self.asm.jmp(fielddone);
        self.asm.place(do_right);
        self.float_emit_pad(b' ');
        self.float_emit_sign();
        self.eg_copy_body(bodybuf);
        self.asm.jmp(fielddone);
        self.asm.place(do_minus);
        self.float_emit_sign();
        self.eg_copy_body(bodybuf);
        self.float_emit_pad(b' ');
        self.asm.place(fielddone);
        self.asm.lea_global(RSI, outbuf);
        self.asm.mov_rr(RDX, R8);
        self.asm.sub_rr(RDX, RSI);
        let l = self.helper(Helper::OutWrite);
        self.asm.call(l);
    }

    /// Copy the body (length r11) from BODYBUF to the OUTBUF cursor r8.
    fn eg_copy_body(&mut self, bodybuf: i32) {
        self.asm.lea_global(RSI, bodybuf);
        self.asm.mov_rr(RDI, R8);
        self.asm.mov_rr(RCX, R11);
        self.asm.rep_movsb();
        self.asm.mov_rr(R8, RDI);
    }
}

/// Detect a print statement: a bare string literal, `"fmt", args…` (a `Comma`
/// whose first element is a string), or `Print("fmt", args…)`. Returns the
/// format string and the argument expressions.
fn as_print(e: &Expr) -> Option<(String, &[Expr])> {
    match &e.kind {
        ExprKind::Comma(items) => match items.first().map(|x| &x.kind) {
            Some(ExprKind::Str(s)) => Some((s.clone(), &items[1..])),
            _ => None,
        },
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind {
                if name == "Print" {
                    if let Some(ExprKind::Str(s)) = args.first().map(|x| &x.kind) {
                        return Some((s.clone(), &args[1..]));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// ---- comparison condition codes (the encoder itself lives in `asm`) ----

/// `setcc` opcode (second byte) for the comparison conditions we emit.
const SETE: u8 = 0x94;
const SETNE: u8 = 0x95;
const SETL: u8 = 0x9C;
const SETGE: u8 = 0x9D;
const SETLE: u8 = 0x9E;
const SETG: u8 = 0x9F;
// Unsigned conditions, used for F64 compares (`ucomisd` sets CF/ZF unsigned-style).
const SETB: u8 = 0x92;
const SETAE: u8 = 0x93;
const SETBE: u8 = 0x96;
const SETA: u8 = 0x97;

/// A scalar that fits a register: an integer, `Bool`, or any pointer.
fn is_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::I8
            | Type::U8
            | Type::I16
            | Type::U16
            | Type::I32
            | Type::U32
            | Type::I64
            | Type::U64
            | Type::Bool
            | Type::Ptr(_)
            | Type::FuncPtr { .. }
    )
}
fn is_array(ty: &Type) -> bool {
    matches!(ty, Type::Array(..))
}
fn is_f64(ty: &Type) -> bool {
    matches!(ty, Type::F64)
}
/// Whether a type is an unsigned integer (drives `>>`/`/`/`%` and float→int).
fn is_unsigned_int(ty: &Type) -> bool {
    matches!(ty, Type::U8 | Type::U16 | Type::U32 | Type::U64)
}
/// An aggregate is represented by its address (it never lives in a register): an
/// array (decays to a pointer) or a `class`/`union` (passed/copied by reference).
fn is_aggregate(ty: &Type) -> bool {
    matches!(ty, Type::Array(..) | Type::Named(_))
}
/// The class/union name of a `Named` type.
fn named_of(ty: &Type, pos: Pos) -> Result<String, CodegenError> {
    match ty {
        Type::Named(n) => Ok(n.clone()),
        _ => Err(CodegenError::at(
            pos,
            "x86_64 backend: member access on a non-class value",
        )),
    }
}
/// The pointee of a pointer (else the type unchanged).
fn pointee(ty: &Type) -> Type {
    match ty {
        Type::Ptr(inner) => (**inner).clone(),
        other => other.clone(),
    }
}
/// Whether `e` is an addressable lvalue (so `gen_addr` applies).
fn is_place(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Ident(_)
            | ExprKind::Index { .. }
            | ExprKind::Member { .. }
            | ExprKind::Unary {
                op: UnOp::Deref,
                ..
            }
    )
}
fn is_brace_init(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::InitList(_) | ExprKind::DesignatedInit(_))
}
/// The pointee/element type of a pointer or array.
fn elem_of(ty: &Type) -> Option<Type> {
    match ty {
        Type::Ptr(inner) | Type::Array(inner, _) => Some((**inner).clone()),
        _ => None,
    }
}
/// Whether a narrow integer load sign-extends (the signed integer types).
fn type_signed(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

/// Opcode bytes (before ModRM) for a width-aware load into rax.
fn load_opcode(size: i32, signed: bool) -> &'static [u8] {
    match (size, signed) {
        (8, _) => &[0x48, 0x8B],           // mov rax, r/m64
        (4, true) => &[0x48, 0x63],        // movsxd rax, r/m32
        (4, false) => &[0x8B],             // mov eax, r/m32 (zero-extends to rax)
        (2, true) => &[0x48, 0x0F, 0xBF],  // movsx rax, r/m16
        (2, false) => &[0x48, 0x0F, 0xB7], // movzx rax, r/m16
        (1, true) => &[0x48, 0x0F, 0xBE],  // movsx rax, r/m8
        (1, false) => &[0x48, 0x0F, 0xB6], // movzx rax, r/m8
        _ => &[0x48, 0x8B],
    }
}
/// Opcode bytes (before ModRM) for storing the low `size` bytes of rax.
fn store_opcode(size: i32) -> &'static [u8] {
    match size {
        8 => &[0x48, 0x89], // mov r/m64, rax
        4 => &[0x89],       // mov r/m32, eax
        2 => &[0x66, 0x89], // mov r/m16, ax
        1 => &[0x88],       // mov r/m8, al
        _ => &[0x48, 0x89],
    }
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::Assign => unreachable!("plain assignment handled separately"),
    }
}

fn align16(n: i32) -> i32 {
    (n + 15) & !15
}
