//! A minimal x86-64 / ELF code-generation backend (Linux).
//!
//! A second native target alongside the [AArch64 backend](super::arm64). It
//! hand-emits x86-64 machine code and writes a **self-contained static ELF
//! executable** — its own `_start` runs the program and calls the `exit` syscall,
//! so there is no linker, no libc, and no relocations. The interpreter remains
//! the conformance oracle.
//!
//! Implemented: top-level code and **functions** (recursion, up to six integer/
//! pointer args via the System V registers), **function pointers** (`&Func` is a
//! RIP-relative `lea`; an indirect call `fp(args)` — incl. fn-pointer params,
//! array elements `ops[i](..)`, class fields `s.m(..)`, and a returned pointer —
//! evaluates the target, spills it, and `call`s through the register, with arg
//! classing driven by the callee's `Type::FuncPtr`), **locals** (a `rbp` frame,
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
//! **brace/designated aggregate initializers** (`gen_init_into`: positional,
//! `.field =` designated/out-of-order, nested, partial, and arrays of classes —
//! for locals and globals alike),
//! **globals** (top-level variables live in a zero-filled BSS region and are
//! reachable from any function), **F64** (SSE2: `xmm0`/`xmm1` as the float
//! result/temp, args in `xmm0..xmm7` and returns in `xmm0`; arithmetic, `-`,
//! comparisons, int↔float conversions (signedness-directed), literals, locals,
//! globals, arrays, params/returns), **printing** — a bare string prints
//! verbatim, `"fmt", …` and `Print("fmt", …)` are printf-style with the full
//! `%[flags][width][.prec]conv` grammar for `%d %i %u %x %X %o %c %s %%`
//! (flags `-0+ #`, `*` width/precision), **`%f`/`%e`/`%g` float printing** —
//! correctly-rounded via a bignum (matches Rust's `{:.P}`/`{:.Pe}` byte-for-byte,
//! including round-half-to-even ties) — and the irreducible **core-library
//! builtins**, lowered with no libc: `MAlloc`/`Free` (an `mmap`-backed bump
//! allocator), SSE `Sqrt`/`Fabs`, and the **sprintf family**
//! (`StrPrint`/`CatPrint`/`MStrPrint` — printf into a buffer via an
//! output sink, see below; the lone string routine still emitted is `StrLen`, used
//! internally by `CatPrint`'s append). The reducible string/memory/ctype/PRNG ops
//! are pure HolyC in `lib/*.hc` now and compile as ordinary functions. The
//! transcendental math builtins
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

    // `MSize` makes `MAlloc` prepend an 8-byte size header; gate it so programs that
    // never call `MSize` keep the lean, header-free heap byte-for-byte.
    cg.uses_msize = crate::ast::program_calls_any(program, &["MSize"]);
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
    heap_bss: Option<(i32, i32)>, // bump allocator's (heap_ptr, heap_end) BSS slots
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
    uses_msize: bool, // program calls `MSize` ⇒ `MAlloc` prepends a size header
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
            heap_bss: None,
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
            uses_msize: false,
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
                            // Brace initialiser (positional or designated): zero the
                            // slot, then store the provided elements/fields (recursing
                            // for nested aggregates) so a partial init leaves the rest 0.
                            Some(init) if is_brace_init(init) => {
                                self.gen_zero(off, size);
                                self.gen_init_into(Place::Local(off), &d.ty, 0, init)?;
                            }
                            Some(_) => {
                                return Err(CodegenError::at(
                                    d.span.pos,
                                    "x86_64 backend: unsupported aggregate initializer",
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
        } else if is_brace_init(init) {
            // Brace initialiser into the (linker-zeroed) BSS global: only the
            // provided elements/fields are written, so a partial init stays zero.
            self.gen_init_into(Place::Global(off), &d.ty, 0, init)?;
        } else {
            return Err(CodegenError::at(
                d.span.pos,
                "x86_64 backend: unsupported global initializer",
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
            ExprKind::Offset { class, path } => {
                // `offset(Class.field…)` is the compile-time byte offset of the
                // (possibly nested) field within the class.
                let off = self.layouts.nested_offset_of(class, path).ok_or_else(|| {
                    CodegenError::at(
                        pos,
                        format!("x86_64 backend: cannot compute offset of `{class}`"),
                    )
                })?;
                self.asm.mov_rax_imm(off as i64);
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
            } => {
                // `&Func` is the function's code address (a function pointer); any
                // other `&lvalue` is the lvalue's address.
                if let ExprKind::Ident(name) = &expr.kind {
                    if self.lookup(name).is_none()
                        && !self.globals.contains_key(name)
                        && let Some(&label) = self.funcs.get(name)
                    {
                        self.asm.lea_rax_label(label);
                        return Ok(());
                    }
                }
                self.gen_addr(expr)?
            }
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
        // Compound `x op= value` on a scalar lvalue. A pointer is a scalar and
        // supports `+=`/`-=` (the rhs is scaled by the pointee size, like `++`);
        // only aggregates/arrays are rejected.
        if !is_scalar(&tty) {
            return Err(CodegenError::at(
                pos,
                "x86_64 backend: compound assignment to a non-scalar is unsupported",
            ));
        }
        self.gen_addr(target)?; // rax = &x
        self.asm.push_rax();
        self.asm.load_through(size, type_signed(&tty)); // rax = current x
        self.asm.push_rax();
        self.gen_expr(value)?; // rax = value
        // `p += n` / `p -= n`: scale the integer rhs by the pointee size.
        if let (Some(elem), AssignOp::Add | AssignOp::Sub) = (elem_of(&tty), op) {
            let stride = self.stride_of(&elem);
            if stride != 1 {
                self.asm.imul_rax_imm32(stride); // rax = value * stride
            }
        }
        self.asm.mov_rcx_rax(); // rcx = (scaled) value
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

    /// `rax = ` the address of byte offset `byte_off` within the aggregate at
    /// `place`.
    fn elem_addr(&mut self, place: Place, byte_off: i32) {
        match place {
            // The slot starts at `rbp - off`; element `byte_off` in is `rbp -
            // (off - byte_off)`.
            Place::Local(off) => self.asm.lea_local(off - byte_off),
            // Globals grow upward in BSS, so the element is `byte_off` higher.
            Place::Global(off) => self.asm.lea_rax_global(off + byte_off),
        }
    }

    /// Emit the stores for a brace initialiser (or a single leaf value) into the
    /// aggregate at `place`, at byte offset `byte_off`. Recurses for nested
    /// arrays/classes; only the provided elements/fields are written (local slots
    /// are zeroed first, globals are linker-zeroed), so partial initialisers leave
    /// the rest zero. Mirrors the arm64 backend's `gen_init_into`.
    fn gen_init_into(
        &mut self,
        place: Place,
        ty: &Type,
        byte_off: i32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        if let ExprKind::InitList(items) = &init.kind {
            match ty {
                Type::Array(elem, _) => {
                    let stride = self.stride_of(elem);
                    for (i, item) in items.iter().enumerate() {
                        self.gen_init_into(place, elem, byte_off + i as i32 * stride, item)?;
                    }
                }
                Type::Named(class) => {
                    let fields: Vec<(Type, i32)> = self
                        .layouts
                        .get(class)
                        .map(|l| {
                            l.fields
                                .iter()
                                .map(|f| (f.ty.clone(), f.offset as i32))
                                .collect()
                        })
                        .unwrap_or_default();
                    for (item, (fty, foff)) in items.iter().zip(fields.iter()) {
                        self.gen_init_into(place, fty, byte_off + foff, item)?;
                    }
                }
                _ => {
                    return Err(CodegenError::at(
                        init.span.pos,
                        "x86_64 backend: an initializer list can only initialize an array, class, or union",
                    ));
                }
            }
            return Ok(());
        }
        if let ExprKind::DesignatedInit(items) = &init.kind {
            let Type::Named(class) = ty else {
                return Err(CodegenError::at(
                    init.span.pos,
                    "x86_64 backend: a designated initializer can only initialize a class or union",
                ));
            };
            // Field name -> (type, offset), captured before the store loop.
            let fields: Vec<(String, Type, i32)> = self
                .layouts
                .get(class)
                .map(|l| {
                    l.fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone(), f.offset as i32))
                        .collect()
                })
                .unwrap_or_default();
            for (name, value) in items {
                let Some((_, fty, foff)) = fields.iter().find(|(n, _, _)| n == name) else {
                    return Err(CodegenError::at(
                        value.span.pos,
                        format!("x86_64 backend: `{class}` has no field `{name}`"),
                    ));
                };
                self.gen_init_into(place, &fty.clone(), byte_off + foff, value)?;
            }
            return Ok(());
        }
        // A leaf value: scalar, pointer, float, or an aggregate-valued expression.
        if is_f64(ty) {
            self.gen_foperand(init)?; // xmm0 = value
            self.elem_addr(place, byte_off); // rax = dest
            self.asm.movsd_store_at(RAX); // [rax] = xmm0
        } else if is_aggregate(ty) {
            let size = self.size_of(ty);
            self.gen_expr(init)?; // rax = source address
            self.asm.push_rax();
            self.elem_addr(place, byte_off); // rax = dest
            self.asm.mov_rdi_rax(); // rdi = dest
            self.asm.pop_rax();
            self.asm.mov_rsi_rax(); // rsi = src
            self.asm.mov_rcx_imm32(size);
            self.asm.rep_movsb();
        } else {
            let size = self.size_of(ty);
            self.gen_int_expr(init, ty)?; // rax = value (float source converts per signedness)
            self.asm.push_rax();
            self.elem_addr(place, byte_off); // rax = dest
            self.asm.mov_rcx_rax(); // rcx = dest
            self.asm.pop_rax(); // rax = value
            self.asm.store_through(size); // [rcx] = rax
        }
        Ok(())
    }

    /// Dispatch a call: a bare name that isn't a variable is a direct
    /// function/builtin call; a function-pointer *variable* (or any computed
    /// callee, e.g. `s.method` or `ops[i]`) is an indirect call.
    fn gen_call(&mut self, callee: &Expr, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        if let ExprKind::Ident(n) = &callee.kind {
            if self.lookup(n).is_none() && !self.globals.contains_key(n) {
                return self.gen_call_by_name(n, args, pos);
            }
        }
        self.gen_indirect_call(callee, args, pos)
    }

    /// Emit a recognized stdlib intrinsic ([`crate::intrinsics`]) inline — a hardware
    /// instruction in place of a call to the function's lib implementation. Returns
    /// whether it was handled; an unhandled name falls through to an ordinary call,
    /// so the lib HolyC body (which the interpreter runs) is the fallback. An
    /// optimization intrinsic computes the same value, so conformance holds.
    fn try_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        _pos: Pos,
    ) -> Result<bool, CodegenError> {
        if crate::intrinsics::kind(name).is_none() || args.len() != 1 {
            return Ok(false);
        }
        // Only optimize a call that resolves to the lib intrinsic — a single F64 arg,
        // F64 result. A user override with a different signature must be called
        // normally (the instruction would leave the result in the wrong register).
        let is_f64_unary = self.funcs_sig.get(name).is_some_and(|(p, r)| {
            matches!(r, Type::F64) && p.len() == 1 && matches!(p[0], Type::F64)
        });
        if !is_f64_unary {
            return Ok(false);
        }
        // SSE2 single-instruction equivalents. (The `Floor`/`Ceil`/`Trunc`/`Round*`
        // intrinsics would need SSE4.1 `roundsd`, which isn't baseline x86-64, so they
        // fall through to the HolyC body in `lib/math.hc`.)
        match name {
            "Sqrt" => {
                self.gen_foperand(&args[0])?; // value in xmm0
                self.asm.sqrtsd(0, 0);
                Ok(true)
            }
            "Fabs" => {
                self.gen_foperand(&args[0])?; // value in xmm0
                self.asm.mov_ri64(RAX, 0x7FFF_FFFF_FFFF_FFFF); // clear the sign bit
                self.asm.movq_xmm_from_r(1, RAX);
                self.asm.andpd(0, 1);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn gen_call_by_name(
        &mut self,
        name: &str,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        // A recognized stdlib intrinsic ([`crate::intrinsics`]) the backend lowers
        // inline (e.g. `Sqrt` → `sqrtsd`) in place of a call to its lib body.
        if self.try_intrinsic(name, args, pos)? {
            return Ok(());
        }
        // A primitive intrinsic (printf family / heap / clock — a lib prototype with
        // no body) is lowered like the old builtins. (`Print` itself reaches the
        // backend via `as_print` at the statement level, not here.)
        if crate::intrinsics::is_primitive(name) {
            return self.gen_builtin(name, args, pos);
        }
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
        let varargs = self.funcs_va.get(name).copied().unwrap_or(false);
        self.emit_call_abi(
            CallTarget::Direct(label),
            &param_tys,
            &ret_ty,
            varargs,
            args,
            pos,
        )
    }

    /// Emit an indirect call through a function-pointer value (`callee`). Its
    /// `FuncPtr` type (from sema) supplies the parameter and return types; a
    /// function pointer is never variadic.
    fn gen_indirect_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let (param_tys, ret_ty) = match self.expr_ty(callee) {
            Type::FuncPtr { ret, params } => (params, *ret),
            _ => {
                return Err(CodegenError::at(
                    pos,
                    "x86_64 backend: called value is not a function pointer",
                ));
            }
        };
        self.emit_call_abi(
            CallTarget::Indirect(callee),
            &param_tys,
            &ret_ty,
            false,
            args,
            pos,
        )
    }

    /// The shared call ABI (System V): pass `args` per `param_tys` — integer/pointer
    /// args in rdi.., F64 args in xmm0.., a class return via an sret pointer in r11 —
    /// then transfer to `target`. A `Direct` target is a known label; an `Indirect`
    /// target's function-pointer value is evaluated and spilled up front so it
    /// survives argument evaluation. `varargs` stages the trailing args into a frame
    /// buffer passed as a (ptr, count) pair.
    fn emit_call_abi(
        &mut self,
        target: CallTarget,
        param_tys: &[Type],
        ret_ty: &Type,
        varargs: bool,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        // For an indirect call, evaluate the function-pointer value first and spill
        // it (deepest on the stack) so it survives argument evaluation; it is popped
        // back into rax just before the `call` (after sret setup, the last use of rax).
        if let CallTarget::Indirect(callee) = target {
            self.gen_expr(callee)?; // rax = function address
            self.asm.push_rax();
        }
        // A class-returning callee uses an sret pointer: the caller allocates a
        // result temp in its frame and hands its address to the callee in r11.
        let sret_slot = if matches!(ret_ty, Type::Named(_)) {
            Some(self.alloc(self.size_of(ret_ty).max(1), self.align_of(ret_ty)))
        } else {
            None
        };
        // For a variadic callee, only the named params are register-passed; the
        // trailing args go into a frame buffer whose address + count are passed as
        // two hidden integer args (see below).
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
        match target {
            CallTarget::Direct(label) => self.asm.call(label),
            CallTarget::Indirect(_) => {
                // The function address was spilled first, so it is on top of the
                // stack now that the args are in registers (rax is free post-sret).
                self.asm.pop_rax();
                self.asm.call_reg(RAX);
            }
        }
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
    /// runtime routines. (The algebraic float ops `Sqrt`/`Fabs` are pure HolyC in
    /// `lib/math.hc` now; the rest — transcendentals, the sprintf family — go through
    /// the lib or aren't supported without libc.)
    fn gen_builtin(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        match name {
            // `Free` is a no-op (the bump allocator never reclaims); still evaluate
            // the argument for its side effects.
            "Free" => {
                self.gen_expr(&args[0])?;
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
            "MAlloc" => self.call_rt("MAlloc", &[&args[0]]),
            "HeapExtend" => self.call_rt("HeapExtend", &[&args[0], &args[1], &args[2]]),
            "MSize" => self.call_rt("MSize", &[&args[0]]),
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
        // Only `MAlloc` (heap) and `StrLen` (used internally by the `CatPrint`
        // append) remain emitted runtime routines; the string/memory/ctype/PRNG
        // ops are now pure HolyC in `lib/*.hc` and compile as ordinary functions.
        const ORDER: &[&str] = &["MAlloc", "HeapExtend", "MSize", "StrLen"];
        for &name in ORDER {
            let Some(&label) = self.rt_routines.get(name) else {
                continue;
            };
            self.asm.place(label);
            match name {
                "MAlloc" => self.emit_rt_malloc(),
                "HeapExtend" => self.emit_rt_heapextend(),
                "MSize" => self.emit_rt_msize(),
                "StrLen" => self.emit_rt_strlen(),
                _ => {}
            }
        }
    }

    /// The bump allocator's `(heap_ptr, heap_end)` BSS slots, allocated once and
    /// shared by `MAlloc` and `HeapExtend`.
    fn heap_globals(&mut self) -> (i32, i32) {
        if let Some(g) = self.heap_bss {
            return g;
        }
        let g = (self.alloc_bss(8, 8), self.alloc_bss(8, 8));
        self.heap_bss = Some(g);
        g
    }

    /// `MAlloc(rdi=n)`: a bump allocator over `mmap`'d chunks (1 MiB, page-aligned),
    /// 16-byte-aligned allocations. `Free` is a no-op, so chunks are never reused.
    fn emit_rt_malloc(&mut self) {
        let (hp, he) = self.heap_globals(); // heap bump pointer, heap end
        let alloc = self.asm.new_label();
        let sized = self.asm.new_label();
        self.asm.emit(&[0x53]); // push rbx (preserve; rbx survives the syscall)
        if self.uses_msize {
            self.asm.emit(&[0x57]); // push rdi — keep the original n for the header
        }
        self.asm.add_ri(RDI, 15);
        self.asm.and_ri(RDI, -16);
        if self.uses_msize {
            self.asm.add_ri(RDI, 16); // reserve a 16-byte size header before the block
        }
        self.asm.mov_rr(RBX, RDI); // rbx = total bytes to bump
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
        if self.uses_msize {
            self.asm.emit(&[0x59]); // pop rcx — the original n
            self.asm.store_qword_at(RAX, RCX); // [base] = n (the size header)
            self.asm.add_ri(RAX, 16); // return base + 16 (past the header)
        }
        self.asm.emit(&[0x5B]); // pop rbx
        self.asm.emit(&[0xC3]); // ret
    }

    /// `HeapExtend(rdi=ptr, rsi=old, rdx=new) -> rax`: if `ptr` is the bump
    /// allocator's last block (it ends exactly at `*heap_ptr`) and growing it to
    /// `new` still fits the chunk, advance `*heap_ptr` and return `ptr`; else NULL.
    /// No copy, no allocation — the move path lives in the HolyC `ReAlloc`.
    fn emit_rt_heapextend(&mut self) {
        let (hp, he) = self.heap_globals();
        let null = self.asm.new_label();
        self.asm.test_rr(RDI, RDI);
        self.asm.je(null); // NULL ptr never extends
        // rax = align16(old), rcx = align16(new)
        self.asm.mov_rr(RAX, RSI);
        self.asm.add_ri(RAX, 15);
        self.asm.and_ri(RAX, -16);
        self.asm.mov_rr(RCX, RDX);
        self.asm.add_ri(RCX, 15);
        self.asm.and_ri(RCX, -16);
        // last block? ptr + align16(old) == *heap_ptr
        self.asm.mov_rr(R8, RDI);
        self.asm.add_rr(R8, RAX); // r8 = block end
        self.asm.lea_global(R9, hp);
        self.asm.load_qword_at(R10, R9); // r10 = *heap_ptr
        self.asm.cmp_reg_reg(R8, R10);
        self.asm.jne(null);
        // fits? ptr + align16(new) <= *heap_end
        self.asm.mov_rr(R8, RDI);
        self.asm.add_rr(R8, RCX); // r8 = ptr + align16(new)
        self.asm.lea_global(R11, he);
        self.asm.load_qword_at(RAX, R11); // rax = *heap_end
        self.asm.cmp_reg_reg(RAX, R8);
        self.asm.jb(null); // *heap_end < ptr + anew ⇒ doesn't fit
        // extend in place: *heap_ptr = ptr + anew; return ptr
        self.asm.store_qword_at(R9, R8);
        if self.uses_msize {
            // Keep the size header current so MSize reflects the grown block.
            self.asm.mov_rr(RCX, RDI);
            self.asm.add_ri(RCX, -16);
            self.asm.store_qword_at(RCX, RDX); // [ptr-16] = new size
        }
        self.asm.mov_rr(RAX, RDI);
        self.asm.emit(&[0xC3]); // ret
        self.asm.place(null);
        self.asm.mov_ri(RAX, 0);
        self.asm.emit(&[0xC3]); // ret (NULL)
    }

    /// `MSize(rdi=ptr) -> rax`: the requested byte size stored in `ptr`'s header
    /// (`*(ptr-16)`), or 0 for NULL. Only emitted when the program uses `MSize`, so
    /// every block carries a header.
    fn emit_rt_msize(&mut self) {
        let nz = self.asm.new_label();
        self.asm.test_rr(RDI, RDI);
        self.asm.jne(nz);
        self.asm.mov_ri(RAX, 0); // MSize(NULL) == 0
        self.asm.emit(&[0xC3]);
        self.asm.place(nz);
        self.asm.mov_rr(RAX, RDI);
        self.asm.add_ri(RAX, -16);
        self.asm.load_qword_at(RAX, RAX); // rax = *(ptr - 16)
        self.asm.emit(&[0xC3]);
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
            // A float-returning call (direct or through a function pointer) leaves
            // its result in xmm0 — the same dispatch as the integer path.
            ExprKind::Call { callee, args } => self.gen_call(callee, args, pos)?,
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
    ///
    /// Helpers request other helpers *during* their own emission: the float
    /// formatters call `OutWrite` and register the bignum sub-routines, and
    /// `OutWrite` calls `GrowSink`. So a single ordered pass can miss a helper first
    /// requested by another helper's body (e.g. a compiled-but-not-top-level
    /// `F64ToStr` that uses `%g` pulls in `OutWrite` only when `FmtFloatEg` is
    /// emitted). Emit in a fixed order, looping until a full pass adds nothing new —
    /// the fixed order keeps the emitted layout deterministic, the loop guarantees
    /// every transitively-requested routine is placed.
    fn emit_helpers(&mut self) {
        let order: [(Helper, fn(&mut Self)); 10] = [
            (Helper::FmtInt, Self::emit_fmt_int),
            (Helper::FmtStr, Self::emit_fmt_str),
            (Helper::FmtFloat, Self::emit_fmt_float),
            (Helper::FmtFloatEg, Self::emit_fmt_float_eg),
            (Helper::BnMul, Self::emit_bn_mul),
            (Helper::BnShr, Self::emit_bn_shr),
            (Helper::BnShl, Self::emit_bn_shl),
            (Helper::BnDiv10, Self::emit_bn_div10),
            (Helper::OutWrite, Self::emit_out_write),
            (Helper::GrowSink, Self::emit_grow_sink),
        ];
        let mut emitted: std::collections::HashSet<Helper> = std::collections::HashSet::new();
        loop {
            let mut progressed = false;
            for &(h, emit) in &order {
                if self.helpers.contains_key(&h) && emitted.insert(h) {
                    let label = self.helpers[&h];
                    self.asm.place(label);
                    emit(self);
                    progressed = true;
                }
            }
            if !progressed {
                break;
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

/// The destination of a brace initialiser: a local frame slot (`rbp - off`) or a
/// BSS global (RIP-relative at `off`). Used by `gen_init_into`/`elem_addr`.
#[derive(Clone, Copy)]
enum Place {
    Local(i32),
    Global(i32),
}

/// The destination of a call: a known function's code `label` (a direct `call
/// rel32`), or a function-pointer value to evaluate and call indirectly.
#[derive(Clone, Copy)]
enum CallTarget<'a> {
    Direct(usize),
    Indirect(&'a Expr),
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
