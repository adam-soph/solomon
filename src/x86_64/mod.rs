//! Minimal x86-64 code-generation backend for Linux.
//!
//! A second native target alongside the [AArch64 backend](super::arm64). It
//! hand-emits x86-64 machine code and writes a self-contained static ELF
//! executable: its own `_start` runs the program and calls the `exit` syscall, so
//! there is no linker, no libc, and no relocations. The interpreter remains the
//! conformance oracle.
//!
//! # Implemented
//!
//! Top-level code and **functions** — recursion, up to six integer/pointer args
//! via the System V registers.
//!
//! **Function pointers**: `&Func` is a RIP-relative `lea`. An indirect call
//! `fp(args)` evaluates the target, spills it, and `call`s through the register,
//! with arg classing driven by the callee's `Type::FuncPtr`. This covers
//! fn-pointer params, array elements `ops[i](..)`, class fields `s.m(..)`, and a
//! returned pointer.
//!
//! **Locals**: an `rbp` frame, `[rbp - off]`.
//!
//! **Control flow**: `if`/`else`, `while`, `for`, `do`, `break`, `continue`,
//! `goto`/labels, and `switch`/`case`/`default` (including `lo ... hi` ranges and
//! the `start:`/`end:` sub-labels), lowered to a compare-chain.
//!
//! **Operators**: comparisons and short-circuit `&&`/`||`/`!`, unary
//! `- + ~ ++ --`, and binary `+ - * / % & | ^ << >>`. `>>`, `/`, `%` and the
//! relational operators are signedness-directed: `sar`/`idiv` vs `shr`/`div`, and
//! signed vs unsigned condition codes.
//!
//! **Pointers & arrays**: `&x`, `*p`, `a[i]` (including 2-D), pointer arithmetic
//! (pointee-scaled) and difference, width-aware narrow loads/stores via
//! `movsx`/`movzx`, and array parameters that decay to a by-reference pointer.
//!
//! **Classes & unions** use `repr(C)` layout from the shared `layout` pass:
//! member access `a.x` and `p->x`, nested fields, `sizeof`, whole-class
//! assignment, member access on a class-returning call (`Mk().x`), arrays of
//! classes, and union aliasing (anonymous-embedded unions resolve through the
//! promoted offset). Class **parameters** are passed by value as a callee-side
//! `rep movsb` copy. Class **returns** use sret: the caller hands a result-temp
//! pointer to the callee in r11.
//!
//! **Brace/designated aggregate initializers** (`gen_init_into`): positional,
//! `.field =` designated/out-of-order, nested, partial, and arrays of classes, for
//! locals and globals alike.
//!
//! **Globals**: top-level variables live in a zero-filled BSS region and are
//! reachable from any function.
//!
//! **F64** (SSE2): `xmm0`/`xmm1` are the float result/temp, args go in `xmm0..xmm7`
//! and returns in `xmm0`. Covers arithmetic, `-`, comparisons, int↔float
//! conversions (signedness-directed), literals, locals, globals, arrays, and
//! params/returns.
//!
//! **Printing**: a bare string prints verbatim; `"fmt", …` and `Print("fmt", …)`
//! are printf-style with the full `%[flags][width][.prec]conv` grammar for
//! `%d %i %u %x %X %o %c %s %%` (flags `-0+ #`, `*` width/precision). The float
//! conversions `%f`/`%e`/`%g` are correctly-rounded via a bignum, matching Rust's
//! `{:.P}`/`{:.Pe}` byte-for-byte including round-half-to-even ties.
//!
//! **Core-library builtins** (the irreducible ones), lowered with no libc:
//! `MAlloc`/`Free` (an `mmap`-backed bump allocator), SSE `Sqrt`/`Fabs`, and the
//! sprintf family `StrPrint`/`CatPrint`/`MStrPrint` (printf into a buffer via the
//! output sink described below). The one string routine still emitted is `StrLen`,
//! used internally by `CatPrint`'s append. The reducible string/memory/ctype/PRNG
//! ops are pure HolyC in `lib/*.hc` and compile as ordinary functions.
//!
//! The transcendental math builtins (`Sin`/`Cos`/`Pow`/`Exp`/`Ln`/…) are
//! deliberately absent. A freestanding static ELF has no libm, and we don't fake
//! them with approximations — like Rust's `core`, which omits them and leaves them
//! to `std` over the platform libm. libm results aren't bit-identical across
//! platforms anyway, since IEEE 754 doesn't require correctly-rounded
//! transcendentals. Otherwise the core-library builtins are complete here.
//! Anything unimplemented is a build-time error.
//!
//! # Evaluation and runtime
//!
//! Expression evaluation is a stack machine. A value lands in `rax`, or `xmm0` for
//! an F64-typed expression. A binary op spills its left operand to the machine
//! stack while the right is computed, so values survive nested calls. An lvalue's
//! address is computed by `gen_addr`, with a width-aware load/store through it.
//!
//! Printing needs no libc and no native formatter. The whole printf family
//! (`Print`/`StrPrint`/`CatPrint`/`MStrPrint` and the `VFmt`/`FmtFloat` core) is pure
//! HolyC in `<stdio.hc>`, auto-included when a program prints; this backend just
//! compiles those bodies and calls them. A bare string prints verbatim and the
//! `"fmt", …` comma form synthesizes a `Print(fmt, …)` call (`gen_print`/`as_print`).
//! The one irreducible leaf is `StdWrite`, lowered per-OS to the `write` syscall — the
//! single sink all of the above bottoms out at.
//!
//! String literals live after the code, RIP-relative addressed. The ELF layout is
//! `[ELF header | one PT_LOAD | code | strings | BSS]`, mapped R+W+X at `0x400000`
//! (`p_memsz > p_filesz` reserves the zero-filled BSS), with `_start` first.

use std::collections::HashMap;

use crate::ast::*;
use crate::codegen::CodegenError;
use crate::layout::{self, Layouts};
use crate::token::{Pos, Span};

mod asm;
mod linux;
mod windows;

use asm::Asm;
pub use linux::X64Linux;
pub use windows::X64Windows;

/// OS-specific policy for the shared x86-64 code generator: the handful of points
/// where the emitted program touches the operating system. Everything else in
/// [`Cg`]/[`Asm`] is OS-agnostic, since the instruction set is the same. The Linux
/// target ([`X64Linux`]) uses raw syscalls in a freestanding static ELF; the
/// Windows target ([`X64Windows`]) calls `kernel32` imports from a self-contained
/// PE. Each seam is a small instruction sequence with a fixed register contract,
/// so the shared `Cg` can drive it without knowing the OS.
///
/// [`FileOp`] is a file-descriptor primitive, dispatched through
/// [`OsTarget::emit_fileop`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileOp {
    Open,
    Read,
    Write,
    Close,
    LSeek,
}

trait OsTarget {
    /// Emit process exit. The exit status is the low 32 bits of `rax`.
    fn emit_exit(&mut self, asm: &mut Asm);

    /// Emit the fresh-chunk grab inside the `MAlloc` bump allocator: allocate `rsi`
    /// zeroed, page-aligned bytes, return the base address in `rax`, and preserve
    /// `rsi`.
    fn emit_page_alloc(&mut self, asm: &mut Asm);

    /// Emit the body of the `StdWrite(fd, buf, n)` primitive: write the `rdx` bytes
    /// at `rsi` to the standard stream named by `rdi` (1 = stdout, 2 = stderr), and
    /// return the bytes written in `rax`. Linux uses a `write` syscall, with the fd
    /// passing straight through. Windows maps the fd to
    /// `GetStdHandle(STD_OUTPUT/ERROR)` and a `WriteFile`.
    fn emit_std_write(&mut self, asm: &mut Asm);

    /// Whether this is a POSIX target (Linux). The Windows PE has no Linux syscalls.
    /// The syscall-group primitives that lack a Windows lowering — sockets,
    /// filesystem mutation, process ids, working dir, threads, futex — are gated on
    /// this flag and rejected with a clear error rather than emitting an invalid
    /// `syscall`.
    fn is_posix(&self) -> bool {
        true
    }

    /// Lower a file fd primitive (`Open`/`Read`/`Write`/`Close`/`LSeek`). Arguments
    /// arrive in the System V registers: `Open` takes rdi=path, rsi=flags, rdx=mode;
    /// `Read`/`Write` take rdi=fd, rsi=buf, rdx=n; `Close` takes rdi=fd; `LSeek`
    /// takes rdi=fd, rsi=off, rdx=whence. The result lands in `rax` — an fd/HANDLE,
    /// byte count, offset, 0, or a negative error. Linux uses the raw syscall.
    /// Windows uses the matching `kernel32` call
    /// (`CreateFileA`/`ReadFile`/`WriteFile`/`CloseHandle`/`SetFilePointerEx`), with
    /// the `fcntl.hc` open flags translated to Win32.
    fn emit_fileop(&mut self, asm: &mut Asm, op: FileOp);

    /// Read the wall clock into `rax` as nanoseconds since the Unix epoch. `scratch`
    /// is a 16-byte BSS slot for the OS time structure. Linux uses
    /// `clock_gettime(CLOCK_REALTIME)`; Windows uses
    /// `GetSystemTimePreciseAsFileTime`.
    fn emit_unix_ns(&mut self, asm: &mut Asm, scratch: i32);

    /// Read the monotonic clock into `rax` as nanoseconds; its origin is
    /// unspecified. Linux uses `clock_gettime(CLOCK_MONOTONIC)`; Windows uses
    /// `GetTickCount64`.
    fn emit_mono_ns(&mut self, asm: &mut Asm, scratch: i32);

    /// Suspend the thread for the nanosecond count in `rax`. Linux uses `nanosleep`;
    /// Windows uses `Sleep`, which has millisecond granularity.
    fn emit_sleep(&mut self, asm: &mut Asm, scratch: i32);

    /// Emit the entry preamble that captures the command line into the BSS slots
    /// `argc_off` and `argv_off`. The `argv_off` slot holds a pointer to the argv
    /// array. Runs just after the entry frame is set up, so the frame pointer `rbp`
    /// is valid, and only when the program uses `ArgC`/`ArgV`. Linux reads them off
    /// the initial stack; Windows builds them from `GetCommandLineA`.
    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32);

    /// Capture the environment pointer (`U8 **envp`) into the BSS slot `envp_off`.
    /// Runs just after the entry frame is set up, so `rbp` is valid, and only when
    /// the program references `EnvP`. Linux computes `&envp[0]` from the initial
    /// stack, just past the argv NULL terminator. Windows has no `envp` array, so it
    /// stores a NULL; env access there is unsupported for now.
    fn emit_capture_env(&mut self, asm: &mut Asm, envp_off: i32);

    /// Package the emitted program into a runnable executable. Takes ownership of
    /// the `Asm` so a policy can read its layout — and, on Windows, append an import
    /// table — before calling [`Asm::finish`]. `bss` is the zero-filled BSS that
    /// follows the image in memory. Linux finishes with no imports and wraps the
    /// blob in an ELF. Windows builds a kernel32 import table, finishes with it, and
    /// wraps the blob in a PE.
    fn wrap(&mut self, asm: Asm, bss: u64) -> Result<Vec<u8>, CodegenError>;
}

/// Compile a type-checked program to a runnable executable image. This driver is
/// OS-independent: it lays out functions and globals, emits the entry, the function
/// bodies, and the print/builtin runtime, then resolves fixups. The four
/// OS-specific steps — exit, page allocation, the stdout sink, and the container
/// format — are deferred to `os`. The concrete targets ([`X64Linux`],
/// [`X64Windows`]) just pick the policy and write the returned bytes to disk.
fn compile(program: &Program, os: Box<dyn OsTarget>) -> Result<Vec<u8>, CodegenError> {
    let (layouts, _) = layout::compute(program);
    let mut cg = Cg::new(layouts, os);

    // Pre-assign a label to every defined function, so calls can forward-reference.
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

    // Top-level variable declarations are globals: BSS-allocated and accessible
    // from any function. Collect them before emitting any code.
    for item in &program.items {
        if let StmtKind::VarDecl { decls } = &item.kind {
            for d in decls {
                cg.declare_global(&d.name, d.ty.clone());
            }
        }
    }

    // `MSize` makes `MAlloc` prepend an 8-byte size header. Gate it so programs that
    // never call `MSize` keep the lean, header-free heap byte-for-byte.
    cg.uses_msize = crate::ast::program_calls_any(program, &["MSize"]);
    // The command line is exposed as the implicit globals `ArgC`/`ArgV`; the entry
    // captures them into these BSS slots. (A `...` function's `VargC`/`VargV` varargs
    // locals are distinct names.) Reserved only when the program references them, so
    // arg-free programs stay byte-for-byte unchanged.
    if crate::ast::program_uses_ident(program, &["ArgC", "ArgV"]) {
        cg.uses_args = true;
        cg.argc_off = Some(cg.alloc_bss(8, 8));
        cg.argv_off = Some(cg.alloc_bss(8, 8));
    }
    // The environment `U8 **EnvP`, captured at the entry. Gated independently of the
    // command line, so an `EnvP`-free program is byte-for-byte unchanged.
    if crate::ast::program_uses_ident(program, &["EnvP"]) {
        cg.uses_env = true;
        cg.envp_off = Some(cg.alloc_bss(8, 8));
    }
    // The implicit task `CTask *Fs` (exception state). A single zero-initialized BSS
    // region whose address is `Fs`; reserved only when the program uses `Fs`/exceptions.
    if crate::ast::program_uses_ident(program, &["Fs"])
        || crate::ast::program_has_exceptions(program)
    {
        cg.uses_exc = true;
        let size = cg.size_of(&Type::Named("CTask".to_string())).max(8);
        cg.ctask_off = Some(cg.alloc_bss(size, 8));
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

    // Builtin runtime routines (MAlloc, string/mem ops, …), only the ones used. The
    // print runtime is gone: the printf family is pure HolyC, an ordinary function.
    cg.emit_rt_routines();

    let bss = cg.bss as u64;
    cg.os.wrap(cg.asm, bss)
}

// ---- code generation ----

/// Number of System V integer argument registers (`gen_call` / parameter setup).
const ARG_REGS: usize = 6;

/// A local's frame location; its lowest byte is at `[rbp - off]`. An array
/// parameter decays to a pointer, so when `indirect` is set the slot holds the
/// *address* of the caller's data rather than the data itself.
#[derive(Clone)]
struct VarLoc {
    off: i32,
    ty: Type,
    indirect: bool,
}

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
const R15: u8 = 15;
const RBP: u8 = 5;
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
/// `ExcFrame` size on x86-64: prev, saved_rsp, saved_rbp, landing_pad (4×8=32) plus
/// the System V callee-saved set saved at `try` / restored by `throw` — rbx, r12, r13,
/// r14, r15 (5×8=40). 72 bytes total.
const EXC_FRAME_SIZE_X86: i32 = 72;

/// A global variable's location: a byte offset into the BSS region. Its address is
/// RIP-relative, resolved like a string reference but past the file image.
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
    depth: i32,     // current frame bump pointer (reclaimed after a call's va buffer)
    max_depth: i32, // high-water mark the frame is sized to
    funcs: HashMap<String, usize>, // name -> code label
    globals: HashMap<String, GlobalLoc>,
    bss: i32,                     // total bytes of BSS allocated (globals + print scratch)
    heap_bss: Option<(i32, i32)>, // bump allocator's (heap_ptr, heap_end) BSS slots
    break_targets: Vec<usize>,
    continue_targets: Vec<usize>,
    ret_label: usize, // `return` jumps here: the epilogue (function) or exit (main)
    cur_ret: Type,    // return type of the function being emitted (drives F64 returns)
    sret_off: Option<i32>, // frame slot holding the sret pointer (a class-returning fn)
    labels: HashMap<String, usize>, // named `goto` labels in the current function
    funcs_sig: HashMap<String, (Vec<Type>, Type)>, // name -> (param types, return type)
    funcs_va: HashMap<String, bool>, // name -> is variadic (`...`)
    rt_routines: HashMap<&'static str, usize>, // builtin runtime routine -> label (on first use)
    // Command-line args: BSS slots holding argc and the argv array pointer, set
    // when the program uses `ArgC`/`ArgV` (the entry captures them). `None` and
    // false for arg-free programs, which then stay byte-for-byte unchanged.
    argc_off: Option<i32>,
    argv_off: Option<i32>,
    uses_args: bool,
    // Environment: a BSS slot holding the `U8 **envp` pointer, set when the program
    // references `EnvP` (the entry captures it). `None` otherwise, so an env-free
    // program is byte-for-byte unchanged.
    envp_off: Option<i32>,
    uses_env: bool,
    uses_msize: bool, // program calls `MSize` ⇒ `MAlloc` prepends a size header
    // Exceptions: a BSS slot holding the process-global `CTask` (the implicit `Fs`),
    // set when the program uses `Fs`/`try`/`throw`. Single-task for now (per-thread TLS
    // via `%fs`/`arch_prctl` is future work, mirroring the freestanding arm64 gap).
    ctask_off: Option<i32>,
    uses_exc: bool,
}

impl Cg {
    fn new(layouts: Layouts, os: Box<dyn OsTarget>) -> Self {
        Cg {
            asm: Asm::new(),
            os,
            layouts,
            scopes: Vec::new(),
            depth: 0,
            max_depth: 0,
            funcs: HashMap::new(),
            globals: HashMap::new(),
            bss: 0,
            heap_bss: None,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            ret_label: 0,
            cur_ret: Type::I64,
            funcs_va: HashMap::new(),
            sret_off: None,
            labels: HashMap::new(),
            funcs_sig: HashMap::new(),
            rt_routines: HashMap::new(),
            argc_off: None,
            argv_off: None,
            uses_args: false,
            envp_off: None,
            uses_env: false,
            uses_msize: false,
            ctask_off: None,
            uses_exc: false,
        }
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

    /// Reserve `size` bytes of zero-initialised BSS at `align`, returning the offset
    /// of its first byte. Its address is the BSS base plus this offset.
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
        // `max_depth` is the high-water mark the frame is sized to. Reclaiming
        // `depth` (a call's transient variadic buffer) therefore never shrinks the
        // frame.
        self.max_depth = self.max_depth.max(self.depth);
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
        // Capture the environment pointer for `EnvP` (independent of the command line).
        if self.uses_env {
            let e = self.envp_off.unwrap();
            self.os.emit_capture_env(&mut self.asm, e);
        }
        // Per-thread exception state (Linux): point the main thread's FS base at the
        // global `CTask` and store its self-pointer, so `Fs` reads it via `fs:[0]`.
        // Spawned threads get their own via the `clone` CLONE_SETTLS path. Gated on
        // use, so non-exception programs are unchanged.
        if self.uses_exc && self.os.is_posix() {
            let off = self.ctask_off.unwrap();
            self.asm.lea_global(RAX, off); // rax = &CTask
            self.asm.lea_global(RCX, off); // rcx = &CTask
            self.asm.store_through(8); // CTask.self = &CTask
            self.asm.mov_ri(RDI, 0x1002); // ARCH_SET_FS
            self.asm.lea_global(RSI, off); // rsi = &CTask
            self.asm.mov_ri(RAX, 158); // SYS_arch_prctl
            self.asm.syscall();
        }
        for item in items {
            match &item.kind {
                // Function/type definitions aren't statements that run here.
                StmtKind::Func(_) | StmtKind::Class(_) => {}
                // A top-level declaration is a global. Its storage is in BSS,
                // already zeroed, so only its initialiser runs, in program order.
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
        self.asm.patch_frame(frame, align16(self.max_depth));
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
        self.collect_labels(body);
        self.asm.place(label);
        let frame = self.asm.prologue();
        // A class-returning function received its result buffer's address in r11.
        // Save it to a frame slot before any code can clobber r11.
        if matches!(ret, Type::Named(_)) {
            let off = self.alloc(8, 8);
            self.asm.mov_rr(RAX, R11);
            self.asm.store_local(off, 8);
            self.sret_off = Some(off);
        }
        // System V classing: integer/pointer/class-pointer args in rdi.., F64 args
        // in xmm0.., the two classes counted independently. Pass 1 spills every
        // incoming argument register to a slot using only `rax`. It can't use
        // `rep movsb` yet, which would clobber later args still in rsi/rdi/rcx.
        // Pass 2 then copies each by-value class from the saved pointer into its
        // local slot.
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
                // A class is passed by value as a pointer. Stash the pointer now,
                // touching only `rax`, and copy it in pass 2.
                let off = self.declare(name, p.ty.clone(), false);
                let ptr_tmp = self.alloc(8, 8);
                self.asm.mov_rax_argreg(gpr);
                self.asm.store_local(ptr_tmp, 8);
                class_copies.push((off, ptr_tmp, self.size_of(&p.ty)));
                gpr += 1;
                continue;
            }
            // An array parameter decays to a pointer (passed by reference). Its
            // slot holds the incoming 8-byte pointer and is marked indirect.
            let indirect = matches!(p.ty, Type::Array(..));
            let off = self.declare(name, p.ty.clone(), indirect);
            self.asm.mov_rax_argreg(gpr);
            // Spill at the slot's width. A narrow scalar (`U8`/`U32`/…) has a
            // sub-8-byte slot, so an 8-byte store would run past it and clobber the
            // adjacent local. The caller already truncated the value to the param
            // width, and reads sign/zero-extend it back, so the low bytes suffice.
            let store_w = if indirect {
                8
            } else {
                self.size_of(&p.ty).clamp(1, 8)
            };
            self.asm.store_local(off, store_w);
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
        // A variadic function takes two hidden integer params after the named ones,
        // in argreg[gpr] and argreg[gpr+1]: the caller's vararg buffer pointer and
        // the count. Spill them to slots and expose them as the implicit HolyC
        // varargs locals `I64 *VargV` (the buffer) and `I64 VargC` (the count).
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
            let scope = self.scopes.last_mut().unwrap();
            scope.insert(
                "VargV".to_string(),
                VarLoc {
                    off: ptr_off,
                    ty: Type::Ptr(Box::new(Type::I64)),
                    indirect: false,
                },
            );
            scope.insert(
                "VargC".to_string(),
                VarLoc {
                    off: cnt_off,
                    ty: Type::I64,
                    indirect: false,
                },
            );
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
        self.asm.patch_frame(frame, align16(self.max_depth));
        Ok(())
    }

    fn begin_function(&mut self) {
        self.scopes = vec![HashMap::new()];
        self.depth = 0;
        self.max_depth = 0;
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
                    self.gen_bare_str(lit, s.span.pos)?;
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
                            // slot, then store the provided elements/fields, recursing
                            // for nested aggregates. A partial init leaves the rest 0.
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
                    // Copy the class result through the saved sret pointer. A
                    // brace/tuple literal (`return a, b;`) is built into a temp first.
                    if let Some(e) = val {
                        let off = self.sret_off.expect("class return needs an sret slot");
                        let size = self.size_of(&ret);
                        if matches!(e.kind, ExprKind::InitList(_)) {
                            let align = self.align_of(&ret);
                            let tmp = self.alloc(size, align);
                            self.gen_init_into(Place::Local(tmp), &ret, 0, e)?;
                            self.asm.lea_local(tmp); // rax = &temp
                        } else {
                            self.gen_expr(e)?; // rax = source address
                        }
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
                        Some(e) => {
                            self.gen_int_expr(e, &ret)?;
                            // C truncates the return value to the declared width for
                            // a narrow `U8`/`I16`/… return. There is no store to do
                            // it, so narrow the register in place, matching the
                            // interpreter and the arm64 backend.
                            self.asm
                                .cast_rax(self.size_of(&ret), !is_unsigned_int(&ret));
                        }
                        None => self.asm.mov_rax_imm(0),
                    }
                }
                self.asm.jmp(self.ret_label);
            }
            StmtKind::If { cond, then, else_ } => {
                crate::backend::gen_if(self, cond, then, else_.as_deref())?
            }
            StmtKind::While { cond, body } => crate::backend::gen_while(self, cond, body)?,
            StmtKind::DoWhile { body, cond } => crate::backend::gen_do_while(self, body, cond)?,
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                crate::backend::gen_for(self, init.as_deref(), cond.as_ref(), step.as_ref(), body)?
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
            StmtKind::Try { body, handler } => self.gen_try(body, handler)?,
            StmtKind::Throw(val) => self.gen_throw(val)?,
            other => {
                return Err(CodegenError::at(
                    s.span.pos,
                    format!("x86_64 backend: unsupported statement {other:?}"),
                ));
            }
        }
        Ok(())
    }

    /// Lower a `switch` via the shared [`crate::backend::gen_switch`] driver; this
    /// backend supplies the leaf emits through [`crate::backend::Emitter`] and uses
    /// the default no-jump-table compare-chain path.
    ///
    /// The discriminant is evaluated once into a frame slot, then a linear
    /// compare-chain dispatches to each `case` label (a single value or a
    /// `lo ... hi` range), falling through to `default`, the epilogue, or the exit
    /// when nothing matches. The optional `start:`/`end:` sub-labels partition the
    /// body into a prologue (runs before dispatch) and an epilogue (reached by
    /// fall-through, skipped by `break`). Mirrors the arm64 backend's compare-chain.
    fn gen_switch(&mut self, cond: &Expr, body: &Stmt, pos: Pos) -> Result<(), CodegenError> {
        crate::backend::gen_switch(self, cond, body, pos)
    }

    /// Byte offset of a `CTask` field (`except_ch`/`catch_except`/`exc_top`).
    fn ctask_field(&self, field: &str) -> i32 {
        self.layouts.offset_of("CTask", field).unwrap_or(0) as i32
    }

    /// Load the current task pointer (`CTask *Fs`) into `reg`. On Linux (freestanding)
    /// it is per-thread: the FS base points at this thread's `CTask`, whose
    /// self-pointer is `fs:[0]` (set at `_start` via `arch_prctl` for the main thread,
    /// and by the `clone` `CLONE_SETTLS` for spawned threads). On Windows (no threads)
    /// it is the address of the process-global `CTask` region.
    fn gen_fs_addr(&mut self, reg: u8) {
        if self.os.is_posix() {
            self.asm.mov_reg_fs0(reg);
        } else {
            self.asm
                .lea_global(reg, self.ctask_off.expect("CTask region"));
        }
    }

    /// Lower `try { body } catch { handler }`. Mirrors the arm64 unwinder: build an
    /// on-stack `ExcFrame` (prev, saved rsp/rbp, landing-pad address, callee-saved set
    /// rbx/r12..r15), push it on `Fs->exc_top`, run the body, and on normal completion
    /// pop it and skip the catch. The `catch` block is the longjmp landing pad. The
    /// `ExcFrame` is a frame local, so its fields are rbp-relative (`store_local`).
    fn gen_try(&mut self, body: &[Stmt], handler: &[Stmt]) -> Result<(), CodegenError> {
        let ef = self.alloc(EXC_FRAME_SIZE_X86, 16); // &ExcFrame = rbp - ef
        let exctop = self.ctask_field("exc_top");
        let catchex = self.ctask_field("catch_except");
        let catch_l = self.asm.new_label();
        let after_l = self.asm.new_label();

        // frame.prev = Fs->exc_top
        self.gen_fs_addr(RAX);
        self.asm.add_rax_imm32(exctop);
        self.asm.load_through(8, false);
        self.asm.store_local(ef, 8);
        // frame.saved_rsp / saved_rbp / landing_pad
        self.asm.mov_rr(RAX, RSP);
        self.asm.store_local(ef - 8, 8);
        self.asm.mov_rr(RAX, RBP);
        self.asm.store_local(ef - 16, 8);
        self.asm.lea_rax_label(catch_l);
        self.asm.store_local(ef - 24, 8);
        // Save the callee-saved set so a `throw` can restore the caller's values.
        for (i, &r) in [RBX, R12, R13, R14, R15].iter().enumerate() {
            self.asm.mov_rr(RAX, r);
            self.asm.store_local(ef - 32 - 8 * i as i32, 8);
        }
        // Fs->exc_top = &ExcFrame
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, exctop);
        self.asm.lea_local(ef); // rax = &ExcFrame
        self.asm.store_through(8);

        for s in body {
            self.gen_stmt(s)?;
        }

        // Normal completion: Fs->exc_top = frame.prev; skip the catch.
        self.asm.load_local(ef, 8, false); // rax = frame.prev
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, exctop);
        self.asm.store_through(8);
        self.asm.jmp(after_l);

        // Landing pad: `throw` jumps here with rsp/rbp/callee-saved already restored,
        // exc_top popped, except_ch set, and catch_except = 1.
        self.asm.place(catch_l);
        for s in handler {
            self.gen_stmt(s)?;
        }
        self.asm.mov_rax_imm(0); // clear catch_except after the handler
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, catchex);
        self.asm.store_through(8);

        self.asm.place(after_l);
        Ok(())
    }

    /// Lower `throw expr;` (or bare `throw;`): set `Fs->except_ch`, then unwind to the
    /// top handler — restore its callee-saved set / rsp / rbp from the `ExcFrame`, pop
    /// it, set `catch_except` = 1, and `jmp` to its landing pad. An empty chain means
    /// the exception is uncaught, which exits the process. `r11` holds the handler
    /// frame across the restores (it is never a restore target).
    fn gen_throw(&mut self, val: &Option<Expr>) -> Result<(), CodegenError> {
        let except = self.ctask_field("except_ch");
        let exctop = self.ctask_field("exc_top");
        let catchex = self.ctask_field("catch_except");

        // rax = value to raise (a bare `throw;` re-raises the current except_ch).
        match val {
            Some(e) => self.gen_int_expr(e, &Type::I64)?,
            None => {
                self.gen_fs_addr(RAX);
                self.asm.add_rax_imm32(except);
                self.asm.load_through(8, false);
            }
        }
        // Fs->except_ch = rax
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, except);
        self.asm.store_through(8);
        // r11 = f = Fs->exc_top
        self.gen_fs_addr(RAX);
        self.asm.add_rax_imm32(exctop);
        self.asm.load_through(8, false);
        self.asm.mov_rr(R11, RAX);
        // Uncaught (f == NULL): exit with the thrown value as the code.
        let live = self.asm.new_label();
        self.asm.test_rr(R11, R11);
        self.asm.jne(live);
        self.gen_fs_addr(RAX);
        self.asm.add_rax_imm32(except);
        self.asm.load_through(8, false);
        self.os.emit_exit(&mut self.asm);
        self.asm.place(live);
        // Restore the callee-saved set from the handler frame, then rsp and rbp.
        for (i, &r) in [RBX, R12, R13, R14, R15].iter().enumerate() {
            self.asm.mov_rr(RAX, R11);
            self.asm.add_rax_imm32(32 + 8 * i as i32);
            self.asm.load_through(8, false);
            self.asm.mov_rr(r, RAX);
        }
        self.asm.mov_rr(RAX, R11);
        self.asm.add_rax_imm32(8); // saved_rsp
        self.asm.load_through(8, false);
        self.asm.mov_rr(RSP, RAX);
        self.asm.mov_rr(RAX, R11);
        self.asm.add_rax_imm32(16); // saved_rbp
        self.asm.load_through(8, false);
        self.asm.mov_rr(RBP, RAX);
        // Fs->exc_top = f.prev (offset 0)
        self.asm.mov_rr(RAX, R11);
        self.asm.load_through(8, false);
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, exctop);
        self.asm.store_through(8);
        // catch_except = 1
        self.asm.mov_rax_imm(1);
        self.gen_fs_addr(RCX);
        self.asm.add_imm32(RCX, catchex);
        self.asm.store_through(8);
        // Jump to the handler's landing pad (offset 24).
        self.asm.mov_rr(RAX, R11);
        self.asm.add_rax_imm32(24);
        self.asm.load_through(8, false);
        self.asm.jmp_reg(RAX);
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
        // means integer context, so convert toward zero (signed). `gen_int_expr`
        // handles an unsigned target.
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
                // integer source just evaluates, with width handled at use/store
                // sites.
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
                // The command line `ArgC` (count) and `ArgV` (the `U8 **` base) are
                // implicit globals captured at the entry, unless a user variable
                // shadows them with a local of the same name.
                "ArgC" | "ArgV" | "EnvP" if self.lookup(name).is_none() => {
                    let off = match name.as_str() {
                        "ArgC" => self.argc_off.expect("ArgC slot reserved"),
                        "ArgV" => self.argv_off.expect("ArgV slot reserved"),
                        _ => self.envp_off.expect("EnvP slot reserved"),
                    };
                    self.asm.lea_rax_global(off);
                    self.asm.load_through(8, false); // rax = ArgC / ArgV / EnvP base
                }
                // `Fs` is the `CTask *` task pointer (per-thread on Linux, global on
                // Windows), unless shadowed by a local.
                "Fs" if self.lookup(name).is_none() && self.ctask_off.is_some() => {
                    self.gen_fs_addr(RAX);
                }
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
                            // A scalar global loads through its address; an
                            // aggregate decays to the address already in rax.
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

    /// `++`/`--` on any scalar lvalue, integer or pointer; a pointer steps by its
    /// pointee size. Addressed through `gen_addr`, so it covers locals, globals,
    /// `*p`, `a[i]`, and members. The result is the new value (prefix) or the
    /// preserved old value (postfix).
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

    /// Apply an arithmetic/bitwise/shift op with `rax = lhs`, `rcx = rhs`. The
    /// `signed` flag, keyed off the left operand's type, directs `/`, `%`, and `>>`:
    /// signed uses `idiv`/`sar`, unsigned uses `div`/`shr`, per C semantics.
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
        // supports `+=`/`-=`, with the rhs scaled by the pointee size like `++`.
        // Only aggregates/arrays are rejected.
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
                // Tuple `t[k]` is positional field access `t._k`.
                if let Some(m) = crate::ast::tuple_index_as_member(e) {
                    return self.gen_addr(&m);
                }
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

    /// Copy `size` bytes from the source address in rax to the local at offset
    /// `dest_off` — a by-value aggregate copy via `rep movsb`.
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

    /// Emit a brace/designated initialiser into the aggregate at `place`. The
    /// recursion and dispatch come from the shared [`crate::backend::gen_init_into`]
    /// driver; this backend supplies the leaf stores via [`crate::backend::Emitter`].
    /// Byte offsets are non-negative, so the driver works in `u32` while this
    /// backend's frame math is `i32`.
    fn gen_init_into(
        &mut self,
        place: Place,
        ty: &Type,
        byte_off: i32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        crate::backend::gen_init_into(self, place, ty, byte_off as u32, init)
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

    /// Emit a recognized stdlib intrinsic ([`crate::intrinsics`]) inline as a
    /// hardware instruction, in place of a call to the function's lib
    /// implementation. Returns whether it was handled; an unhandled name falls
    /// through to an ordinary call, so the lib HolyC body (which the interpreter
    /// runs) is the fallback. An optimization intrinsic computes the same value, so
    /// conformance holds.
    fn try_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        _pos: Pos,
    ) -> Result<bool, CodegenError> {
        if crate::intrinsics::kind(name).is_none() || args.len() != 1 {
            return Ok(false);
        }
        // Only optimize a call that resolves to the lib intrinsic: a single F64 arg,
        // F64 result. A user override with a different signature must be called
        // normally, since the instruction would leave the result in the wrong
        // register.
        let is_f64_unary = self.funcs_sig.get(name).is_some_and(|(p, r)| {
            matches!(r, Type::F64) && p.len() == 1 && matches!(p[0], Type::F64)
        });
        if !is_f64_unary {
            return Ok(false);
        }
        // SSE2 single-instruction equivalents. The `Floor`/`Ceil`/`Trunc`/`Round*`
        // intrinsics would need SSE4.1 `roundsd`, which isn't baseline x86-64, so
        // they fall through to the HolyC body in `lib/math.hc`.
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
        // A primitive intrinsic — the printf family, heap, clock, sockets, threads:
        // a lib prototype with no body — is lowered like the old builtins. The
        // exception is when a compiled user function shadows the name (a program's
        // own `Join`/`Read`): `funcs` holds only real definitions, so its presence
        // means "call the body." (`Print` itself reaches the backend via `as_print`,
        // not here.)
        if !self.funcs.contains_key(name) && crate::intrinsics::is_primitive(name) {
            return self.gen_builtin(name, args, pos);
        }
        // Reached for an ordinary defined function. The x86-64 backend has no libc,
        // so any builtin would have been lowered inline above, emitted from scratch.
        let label = match self.funcs.get(name) {
            Some(&l) => l,
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

    /// The shared call ABI (System V). Passes `args` per `param_tys`:
    /// integer/pointer args in rdi.., F64 args in xmm0.., and a class return via an
    /// sret pointer in r11. Then transfers to `target`. A `Direct` target is a known
    /// label; an `Indirect` target's function-pointer value is evaluated and spilled
    /// up front so it survives argument evaluation. `varargs` stages the trailing
    /// args into a frame buffer passed as a (ptr, count) pair.
    fn emit_call_abi(
        &mut self,
        target: CallTarget,
        param_tys: &[Type],
        ret_ty: &Type,
        varargs: bool,
        args: &[Expr],
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let indirect_callee = if let CallTarget::Indirect(c) = target {
            Some(c)
        } else {
            None
        };
        // For a variadic callee, only the named params are register-passed; trailing
        // args become the variadic `extra`. A missing param type — a malformed
        // over-long call — defaults to I64.
        let n_named = if varargs {
            param_tys.len().min(args.len())
        } else {
            args.len()
        };
        let fallback = Type::I64;
        let named: Vec<(&Type, &Expr)> = (0..n_named)
            .map(|i| (param_tys.get(i).unwrap_or(&fallback), &args[i]))
            .collect();
        let extra: Vec<&Expr> = args[n_named..].iter().collect();
        // F64 → xmm0.., everything else → GP rdi.. (System V: 6 integer, 8 float).
        let classes = crate::backend::classify_args(named.iter().map(|(t, _)| *t), ARG_REGS, 8)
            .map_err(|o| {
                CodegenError::at(
                    pos,
                    match o {
                        crate::backend::ArgOverflow::Int => {
                            "x86_64 backend: at most 6 integer arguments"
                        }
                        crate::backend::ArgOverflow::Float => {
                            "x86_64 backend: at most 8 F64 arguments"
                        }
                    },
                )
            })?;
        crate::backend::gen_call(
            self,
            indirect_callee,
            &named,
            &extra,
            &classes,
            ret_ty,
            varargs,
            pos,
            |cg| match target {
                CallTarget::Direct(label) => cg.asm.call(label),
                CallTarget::Indirect(_) => {
                    // The callee was spilled first, so it is on top of the stack now
                    // that the args are in registers (rax is free post-sret).
                    cg.asm.pop_rax();
                    cg.asm.call_reg(RAX);
                }
            },
        )
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

    /// Freestanding `Thread(fn, arg)`: spawn a real `CLONE_THREAD` thread via
    /// `clone(2)` onto an `mmap`'d stack running `fn(arg)`. A 32-byte thread control
    /// block (TCB) at the stack base — `[retval | ctid futex | fn | arg]` — passes
    /// the closure in and carries the result back; its address is the handle.
    /// `CLONE_PARENT_SETTID` writes the new tid into the futex word synchronously, so
    /// `Join` can't race a not-yet-set word, and `CLONE_CHILD_CLEARTID` zeroes it and
    /// futex-wakes on exit. The child inherits `rbp` and the shared VM, so it reads
    /// the TCB pointer back from the frame slot the parent stored it in.
    fn gen_thread(&mut self, args: &[Expr]) -> Result<(), CodegenError> {
        const SIZE: i32 = 0x2_0000; // 128 KiB stack + TCB
        // The exact flag set glibc's pthread_create uses: CLONE_VM|FS|FILES|SIGHAND|
        // THREAD|SYSVSEM|SETTLS|PARENT_SETTID|CHILD_CLEARTID. The emulated x86-64
        // runtime (Rosetta/qemu) rejects thread clones without SYSVSEM and a valid
        // TLS whose first word self-points, so we set both even though the compiled
        // code uses no TLS.
        const FLAGS: i32 = 0x3D_0F00;
        const TLS_OFF: i32 = 0x40; // a TLS self-pointer slot, just past the 32-byte TCB
        let base_slot = self.alloc(8, 8);
        // mmap(0, SIZE, PROT_READ|WRITE, MAP_PRIVATE|ANON, -1, 0) -> rax = base.
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RSI, SIZE);
        self.asm.mov_ri(RDX, 3); // PROT_READ|PROT_WRITE
        self.asm.mov_ri(R10, 0x22); // MAP_PRIVATE|MAP_ANONYMOUS
        self.asm.mov_ri(R8, -1);
        self.asm.mov_ri(R9, 0);
        self.asm.mov_ri(RAX, 9); // mmap
        self.asm.syscall();
        self.asm.store_local(base_slot, 8); // base_slot = base
        // TCB.fn / TCB.arg (push the evaluated value, recover base, store at offset).
        for (e, tcb_off) in [(&args[0], 16i32), (&args[1], 24)] {
            self.gen_int_expr(e, &Type::I64)?; // rax = fn / arg
            self.asm.push_rax();
            self.asm.load_local(base_slot, 8, false); // rax = base
            self.asm.mov_rr(RCX, RAX);
            self.asm.add_ri(RCX, tcb_off); // rcx = base + off
            self.asm.pop_rax(); // rax = value
            self.asm.store_qword_at(RCX, RAX); // [base+off] = value
        }
        // TLS self-pointer: [base+TLS_OFF] = base+TLS_OFF (x86-64 thread-pointer ABI).
        self.asm.load_local(base_slot, 8, false);
        self.asm.mov_rr(RCX, RAX);
        self.asm.add_ri(RCX, TLS_OFF); // rcx = &TLS
        self.asm.store_qword_at(RCX, RCX); // [&TLS] = &TLS
        let l_child = self.asm.new_label();
        let l_done = self.asm.new_label();
        // `base` rides into the child in callee-saved rbx, which the child inherits
        // along with the register file. The frame slot is shared and the parent
        // overwrites it on the next spawn, so a concurrent child must NOT re-read
        // base from it. Save the caller's rbx to a frame slot and restore it on the
        // parent path.
        let rbx_slot = self.alloc(8, 8);
        self.asm.mov_rr(RAX, RBX);
        self.asm.store_local(rbx_slot, 8); // rbx_slot = caller's rbx
        self.asm.load_local(base_slot, 8, false);
        self.asm.mov_rr(RBX, RAX); // rbx = base (captured into the child)
        // clone(FLAGS, child_stack_top, ptid=&futex, ctid=&futex, tls=&TLS). x86-64 arg
        // order is (flags, stack, ptid, ctid, tls).
        self.asm.mov_rr(RSI, RBX);
        self.asm.add_ri(RSI, SIZE - 16); // rsi = child stack top
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 8); // rdx = ptid = &futex
        self.asm.mov_rr(R10, RBX);
        self.asm.add_ri(R10, 8); // r10 = ctid = &futex
        self.asm.mov_rr(R8, RBX);
        self.asm.add_ri(R8, TLS_OFF); // r8 = tls = &TLS self-pointer
        self.asm.mov_ri(RDI, FLAGS);
        self.asm.mov_ri(RAX, 56); // clone
        self.asm.syscall();
        self.asm.test_rax();
        self.asm.je(l_child);
        // Parent: rax = child tid (the futex tracks liveness). Restore rbx; the
        // result is `base`.
        self.asm.load_local(rbx_slot, 8, false);
        self.asm.mov_rr(RBX, RAX); // restore caller's rbx
        self.asm.load_local(base_slot, 8, false); // rax = base (handle)
        self.asm.jmp(l_done);
        // Child: rax == 0, rbx = base. Read fn/arg from the TCB and run it. rbx is
        // callee-saved, so it survives the call and still holds base afterward.
        self.asm.place(l_child);
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 24);
        self.asm.load_qword_at(RDI, RDX); // rdi = arg
        self.asm.mov_rr(RDX, RBX);
        self.asm.add_ri(RDX, 16);
        self.asm.load_qword_at(RAX, RDX); // rax = fn
        self.asm.call_reg(RAX); // fn(arg); rax = return
        self.asm.store_qword_at(RBX, RAX); // [base+0] = return
        self.asm.mov_ri(RDI, 0);
        self.asm.mov_ri(RAX, 60); // exit (this thread; fires CLONE_CHILD_CLEARTID)
        self.asm.syscall();
        self.asm.place(l_done);
        Ok(())
    }

    /// Freestanding `Join(handle)`: futex-wait on the TCB's `ctid` word until the
    /// kernel clears it (thread exit), then return the stashed `retval`.
    fn gen_join(&mut self, args: &[Expr]) -> Result<(), CodegenError> {
        let base_slot = self.alloc(8, 8);
        self.gen_int_expr(&args[0], &Type::I64)?; // rax = handle (TCB base)
        self.asm.store_local(base_slot, 8);
        let l_wait = self.asm.new_label();
        let l_done = self.asm.new_label();
        self.asm.place(l_wait);
        self.asm.load_local(base_slot, 8, false); // rax = base
        self.asm.mov_rr(RCX, RAX);
        self.asm.add_ri(RCX, 8); // rcx = &futex
        self.asm.load_qword_at(RAX, RCX); // rax = *futex (high 4 bytes stay 0)
        self.asm.test_rax();
        self.asm.je(l_done);
        // futex(&futex, FUTEX_WAIT=0, val=observed tid, timeout=NULL).
        self.asm.mov_rr(RDX, RAX); // rdx = val
        self.asm.load_local(base_slot, 8, false);
        self.asm.mov_rr(RDI, RAX);
        self.asm.add_ri(RDI, 8); // rdi = &futex
        self.asm.mov_ri(RSI, 0); // FUTEX_WAIT
        self.asm.mov_ri(R10, 0); // timeout = NULL
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        self.asm.jmp(l_wait);
        self.asm.place(l_done);
        self.asm.load_local(base_slot, 8, false); // rax = base
        self.asm.load_qword_at(RAX, RAX); // rax = [base+0] = retval
        Ok(())
    }

    /// Lower `FutexWait(addr, val)` / `FutexWake(addr, n)` to the Linux `futex(2)`
    /// syscall (`FUTEX_WAIT` 0 / `FUTEX_WAKE` 1) on the low 32 bits of `*addr`. A
    /// `FutexWait` carries a short relative timeout (a `struct timespec` on the
    /// stack), so a missed wakeup re-checks rather than deadlocks — relying on the
    /// documented spurious-wakeup contract.
    fn gen_futex(&mut self, name: &str, args: &[Expr]) -> Result<(), CodegenError> {
        // ≈1 ms — never reached when wakeups work.
        const FUTEX_TIMEOUT_NS: i32 = 1_000_000;
        let wake = name == "FutexWake";
        self.gen_int_expr(&args[1], &Type::I64)?; // val (expected / n)
        self.asm.push_rax();
        self.gen_int_expr(&args[0], &Type::I64)?; // addr
        self.asm.mov_rr(RDI, RAX); // rdi = uaddr
        self.asm.pop_rax();
        self.asm.mov_rr(RDX, RAX); // rdx = val
        self.asm.mov_ri(RSI, if wake { 1 } else { 0 }); // FUTEX_WAKE / FUTEX_WAIT
        if wake {
            self.asm.mov_ri(R10, 0); // no timeout
        } else {
            // Relative `struct timespec {0, FUTEX_TIMEOUT_NS}` on the stack -> r10.
            self.asm.add_ri(RSP, -16);
            self.asm.store_rsp_imm(0, 0); // tv_sec
            self.asm.store_rsp_imm(8, FUTEX_TIMEOUT_NS); // tv_nsec
            self.asm.mov_rr(R10, RSP); // r10 = &timespec
        }
        self.asm.mov_ri(R8, 0); // uaddr2
        self.asm.mov_ri(R9, 0); // val3
        self.asm.mov_ri(RAX, 202); // SYS_futex
        self.asm.syscall();
        if !wake {
            self.asm.add_ri(RSP, 16);
        }
        Ok(())
    }

    /// Lower an atomic op (`atomic.hc`), width-directed by the pointer's pointee type
    /// (1/2/4/8 bytes). On x86-64 a plain aligned `mov` already gives an atomic
    /// acquire load / release store; add/swap/cas use the `lock`-prefixed
    /// `xadd`/`xchg`/`cmpxchg`. The loaded value is sign/zero-extended to the pointee
    /// width by `cast_rax`. The result lands in rax.
    fn gen_atomic(&mut self, name: &str, args: &[Expr]) -> Result<(), CodegenError> {
        let pty = match self.expr_ty(&args[0]) {
            Type::Ptr(inner) | Type::Array(inner, _) => *inner,
            _ => Type::I64,
        };
        let w = match self.size_of(&pty) {
            n @ (1 | 2 | 4) => n,
            _ => 8,
        };
        let signed = !is_unsigned_int(&pty);
        match name {
            "AtomicLoad" => {
                self.gen_int_expr(&args[0], &Type::I64)?; // rax = p
                self.asm.load_through(w, signed); // rax = [p], sign/zero-extended
            }
            "AtomicStore" => {
                self.gen_int_expr(&args[1], &Type::I64)?; // v
                self.asm.push_rax();
                self.gen_int_expr(&args[0], &Type::I64)?; // p
                self.asm.mov_rr(RCX, RAX); // rcx = p (store_through writes [rcx])
                self.asm.pop_rax(); // rax = v
                self.asm.store_through(w); // [p] = low `w` bytes of v
            }
            "AtomicAdd" => {
                self.gen_int_expr(&args[1], &Type::I64)?; // delta
                self.asm.push_rax();
                self.gen_int_expr(&args[0], &Type::I64)?; // p
                self.asm.mov_rsi_rax(); // rsi = p
                self.asm.pop_rax(); // rax = delta
                self.asm.mov_rr(RDX, RAX); // rdx = delta (kept past the xadd)
                self.asm.lock_xadd(RSI, RAX, w); // rax = old, [rsi] += delta
                self.asm.cast_rax(w, signed); // extend old (correct signed-narrow add)
                self.asm.add_rr(RAX, RDX); // rax = old + delta = new
                self.asm.cast_rax(w, signed); // truncate+extend new for the return
            }
            "AtomicSwap" => {
                self.gen_int_expr(&args[1], &Type::I64)?; // v
                self.asm.push_rax();
                self.gen_int_expr(&args[0], &Type::I64)?; // p
                self.asm.mov_rsi_rax(); // rsi = p
                self.asm.pop_rax(); // rax = v
                self.asm.xchg_mem(RSI, RAX, w); // rax = old, [rsi] = v
                self.asm.cast_rax(w, signed);
            }
            "AtomicCas" => {
                self.gen_int_expr(&args[2], &Type::I64)?; // desired
                self.asm.push_rax();
                self.gen_int_expr(&args[1], &Type::I64)?; // expected
                self.asm.push_rax();
                self.gen_int_expr(&args[0], &Type::I64)?; // p
                self.asm.mov_rsi_rax(); // rsi = p
                self.asm.pop_rax(); // rax = expected (the cmpxchg comparand)
                self.asm.pop_rcx(); // rcx = desired
                self.asm.lock_cmpxchg(RSI, RCX, w); // if [rsi]==acc then [rsi]=rcx; acc=old
                self.asm.cast_rax(w, signed); // extend the witnessed old for the return
            }
            _ => unreachable!(),
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
    /// runtime routines. The algebraic float ops `Sqrt`/`Fabs` are pure HolyC in
    /// `lib/math.hc` now; the rest — transcendentals, the sprintf family — go through
    /// the lib or aren't supported without libc.
    fn gen_builtin(&mut self, name: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        match name {
            // `Free` is a no-op, since the bump allocator never reclaims. Still
            // evaluate the argument for its side effects.
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
            // The printf family `Print`/`StrPrint`/`CatPrint`/`MStrPrint` is pure
            // HolyC now (`lib/stdio.hc`), so it is compiled and called like any
            // function, not lowered here.
            // Portable standard-stream write (fd 1 = stdout, 2 = stderr). Unlike the
            // Linux-only raw-syscall `Write` below, this goes through the OS seam, so
            // it also works on Windows (`WriteFile` via `GetStdHandle`). Args land in
            // rdi=fd, rsi=buf, rdx=n; `emit_std_write` returns the bytes written in
            // rax.
            "StdWrite" => {
                let arg_refs: Vec<&Expr> = args.iter().collect();
                self.gen_int_args(&arg_refs)?;
                self.os.emit_std_write(&mut self.asm);
                Ok(())
            }
            // File fd primitives, lowered through the OS seam (Linux syscall, Windows
            // `kernel32`), so they work on both targets. Args are in the System V
            // registers; the result is in rax — an fd/HANDLE, byte count, offset, 0,
            // or a negative error.
            "Open" | "LSeek" | "Read" | "Write" | "Close" => {
                let op = match name {
                    "Open" => FileOp::Open,
                    "Read" => FileOp::Read,
                    "Write" => FileOp::Write,
                    "Close" => FileOp::Close,
                    _ => FileOp::LSeek,
                };
                let arg_refs: Vec<&Expr> = args.iter().collect();
                self.gen_int_args(&arg_refs)?;
                self.os.emit_fileop(&mut self.asm, op);
                Ok(())
            }
            // POSIX-only primitives via raw Linux syscalls (x86-64 numbers: socket
            // 41, connect 42, unlink 87, rename 82, mkdir 83, getpid 39, …). Each
            // takes ≤3 args, so the System V arg registers (rdi/rsi/rdx) coincide
            // with the syscall registers, and the result is in rax. There is no
            // Windows lowering for these yet, so reject them with a clear error on the
            // PE target rather than emitting an invalid `syscall`.
            "Socket" | "Connect" | "Remove" | "Rename" | "Mkdir" | "Getpid" | "Getppid"
            | "Getuid" | "Getgid" | "Chdir" => {
                if !self.os.is_posix() {
                    return Err(CodegenError::at(
                        pos,
                        format!(
                            "x86_64 backend: `{name}` is not supported on the Windows target yet"
                        ),
                    ));
                }
                let nr: i32 = match name {
                    "Getpid" => 39,
                    "Chdir" => 80,
                    "Rename" => 82,
                    "Mkdir" => 83,
                    "Remove" => 87,
                    "Getuid" => 102,
                    "Getgid" => 104,
                    "Getppid" => 110,
                    "Socket" => 41,
                    "Connect" => 42,
                    _ => unreachable!(),
                };
                let arg_refs: Vec<&Expr> = args.iter().collect();
                self.gen_int_args(&arg_refs)?;
                self.asm.mov_ri(RAX, nr);
                self.asm.syscall();
                Ok(())
            }
            // `Getcwd(buf, size)`: the `getcwd` syscall (79) returns the byte length
            // on success or `-errno`. Normalise a non-negative result to 0, matching
            // the 0/-errno contract of the other working-dir ops.
            "Getcwd" => {
                if !self.os.is_posix() {
                    return Err(CodegenError::at(
                        pos,
                        "x86_64 backend: `Getcwd` is not supported on the Windows target yet",
                    ));
                }
                let arg_refs: Vec<&Expr> = args.iter().collect();
                self.gen_int_args(&arg_refs)?; // rdi = buf, rsi = size
                self.asm.mov_ri(RAX, 79);
                self.asm.syscall(); // rax = len(>0) or -errno
                let neg = self.asm.new_label();
                self.asm.cmp_ri(RAX, 0);
                self.asm.js(neg); // negative -errno passes through
                self.asm.xor_rr(RAX, RAX); // a length → 0
                self.asm.place(neg);
                Ok(())
            }
            // Terminate the process with `code` (rax); the OS seam emits the Linux
            // `exit`/`exit_group` syscall or Windows `ExitProcess`.
            "Exit" => {
                self.gen_expr(&args[0])?; // rax = code
                self.os.emit_exit(&mut self.asm);
                Ok(())
            }
            // Threads and the futex behind blocking sync are raw Linux clone/futex
            // syscalls — no Windows lowering yet.
            "Thread" | "Join" | "FutexWait" | "FutexWake" if !self.os.is_posix() => {
                Err(CodegenError::at(
                    pos,
                    format!("x86_64 backend: `{name}` is not supported on the Windows target yet"),
                ))
            }
            "Thread" => self.gen_thread(args),
            "Join" => self.gen_join(args),
            "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas" => {
                self.gen_atomic(name, args)
            }
            "AtomicFence" => {
                self.asm.mfence();
                Ok(())
            }
            "FutexWait" | "FutexWake" => self.gen_futex(name, args),
            "MAlloc" => self.call_rt("MAlloc", &[&args[0]]),
            "HeapExtend" => self.call_rt("HeapExtend", &[&args[0], &args[1], &args[2]]),
            "MSize" => self.call_rt("MSize", &[&args[0]]),
            other => Err(CodegenError::at(
                pos,
                format!("x86_64 backend: builtin `{other}` is not supported yet"),
            )),
        }
    }

    /// Emit the bodies of the builtin runtime routines actually used (in a fixed
    /// order for reproducibility). Each follows the internal ABI: arguments in
    /// rdi/rsi/rdx/rcx, result in rax, clobbering only volatile registers.
    fn emit_rt_routines(&mut self) {
        // Only `MAlloc` (heap) and `StrLen` (used internally by the `CatPrint`
        // append) remain emitted runtime routines. The string/memory/ctype/PRNG ops
        // are now pure HolyC in `lib/*.hc` and compile as ordinary functions.
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
    /// `f as u64`: negatives and NaN give 0, and values ≥ 2^63 are handled by a bias
    /// split. Extreme saturation beyond 2^64 is not modelled — a documented edge.
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

    /// Lower a printf-style print of the literal format `fmt` with `args`. Printing
    /// is the pure-HolyC `Print` now, auto-included via `<stdio.hc>` when a program
    /// prints, so this synthesizes `Print(fmt, args…)` and emits it as an ordinary
    /// call to the compiled body. Target-independent (Linux ELF and Windows PE): the
    /// HolyC `Print` ultimately calls `StdWrite`, which the OS seam lowers per
    /// target.
    fn gen_print(&mut self, fmt: &str, args: &[Expr], pos: Pos) -> Result<(), CodegenError> {
        let fmt_expr = Expr::new(ExprKind::Str(fmt.to_string()), Span::dummy());
        fmt_expr.set_ty(Type::Ptr(Box::new(Type::U8)));
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(fmt_expr);
        call_args.extend(args.iter().cloned());
        self.gen_call_by_name("Print", &call_args, pos)
    }

    /// A bare string statement prints verbatim — no `%` processing, matching the
    /// interpreter — so it lowers to `StdWrite(STDOUT, lit, len)` rather than
    /// `Print`.
    fn gen_bare_str(&mut self, lit: &str, pos: Pos) -> Result<(), CodegenError> {
        if lit.is_empty() {
            return Ok(());
        }
        let s = Expr::new(ExprKind::Str(lit.to_string()), Span::dummy());
        s.set_ty(Type::Ptr(Box::new(Type::U8)));
        let fd = Expr::new(ExprKind::Int(1), Span::dummy()); // STDOUT
        fd.set_ty(Type::I64);
        let n = Expr::new(ExprKind::Int(lit.len() as i64), Span::dummy());
        n.set_ty(Type::I64);
        self.gen_call_by_name("StdWrite", &[fd, s, n], pos)
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
/// An aggregate is represented by its address and never lives in a register: an
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

/// The single shared-driver emitter vtable ([`crate::backend::Emitter`]) for this
/// backend. It supplies the leaf emits for the initializer lowering
/// ([`gen_init_into`]), the control-flow drivers (the loops, conditionals, and
/// `switch` — whose slot is a frame offset, using the default no-jump-table
/// compare-chain), and the call driver ([`crate::backend::gen_call`]). The driver's
/// `u32` byte offsets are cast to this backend's `i32` frame math.
impl crate::backend::Emitter for Cg {
    type Place = Place;
    type Slot = i32;

    fn backend_label(&self) -> &'static str {
        "x86_64 backend"
    }

    fn init_layouts(&self) -> &Layouts {
        &self.layouts
    }

    fn emit_float_init(
        &mut self,
        place: Place,
        byte_off: u32,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        self.gen_foperand(init)?; // xmm0 = value
        self.elem_addr(place, byte_off as i32); // rax = dest
        self.asm.movsd_store_at(RAX); // [rax] = xmm0
        Ok(())
    }

    fn emit_aggregate_init(
        &mut self,
        place: Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        let size = self.size_of(ty);
        self.gen_expr(init)?; // rax = source address
        self.asm.push_rax();
        self.elem_addr(place, byte_off as i32); // rax = dest
        self.asm.mov_rdi_rax(); // rdi = dest
        self.asm.pop_rax();
        self.asm.mov_rsi_rax(); // rsi = src
        self.asm.mov_rcx_imm32(size);
        self.asm.rep_movsb();
        Ok(())
    }

    fn emit_scalar_init(
        &mut self,
        place: Place,
        byte_off: u32,
        ty: &Type,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        let size = self.size_of(ty);
        self.gen_int_expr(init, ty)?; // rax = value (float source converts per signedness)
        self.asm.push_rax();
        self.elem_addr(place, byte_off as i32); // rax = dest
        self.asm.mov_rcx_rax(); // rcx = dest
        self.asm.pop_rax(); // rax = value
        self.asm.store_through(size); // [rcx] = rax
        Ok(())
    }

    fn new_label(&mut self) -> usize {
        self.asm.new_label()
    }
    fn place_label(&mut self, l: usize) {
        self.asm.place(l);
    }
    fn branch(&mut self, l: usize) {
        self.asm.jmp(l);
    }
    fn push_break(&mut self, l: usize) {
        self.break_targets.push(l);
    }
    fn pop_break(&mut self) {
        self.break_targets.pop();
    }
    fn push_continue(&mut self, l: usize) {
        self.continue_targets.push(l);
    }
    fn pop_continue(&mut self) {
        self.continue_targets.pop();
    }
    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn exit_scope(&mut self) {
        self.scopes.pop();
    }
    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), CodegenError> {
        self.gen_stmt(s)
    }
    fn branch_if_false(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError> {
        self.gen_cond_jump_false(cond, label)
    }
    fn branch_if_true(&mut self, cond: &Expr, label: usize) -> Result<(), CodegenError> {
        self.gen_expr(cond)?;
        self.asm.test_rax();
        self.asm.jne(label);
        Ok(())
    }
    fn eval_expr_discard(&mut self, e: &Expr) -> Result<(), CodegenError> {
        self.gen_expr(e)
    }

    fn eval_switch_value(&mut self, cond: &Expr) -> Result<i32, CodegenError> {
        self.gen_expr(cond)?; // rax = switch value
        let voff = self.alloc(8, 8);
        self.asm.store_local(voff, 8);
        Ok(voff)
    }

    fn switch_cmp_branch(
        &mut self,
        slot: i32,
        bound: &Expr,
        cc: crate::backend::SwitchCc,
        target: usize,
    ) -> Result<(), CodegenError> {
        self.gen_expr(bound)?; // rax = bound
        self.asm.mov_rcx_rax(); // rcx = bound
        self.asm.load_local(slot, 8, false); // rax = v
        self.asm.cmp_rax_rcx();
        match cc {
            crate::backend::SwitchCc::Eq => self.asm.je(target),
            crate::backend::SwitchCc::Lt => self.asm.jl(target),
            crate::backend::SwitchCc::Gt => self.asm.jg(target),
        }
        Ok(())
    }

    // Call-driver leaves: the sret slot is a frame offset. Variadics are marshalled
    // by pushing the buffer address and count on the stack, then popping them into
    // registers before the named args.
    fn spill_callee(&mut self, callee: &Expr) -> Result<(), CodegenError> {
        self.gen_expr(callee)?; // rax = function address
        self.asm.push_rax();
        Ok(())
    }

    fn alloc_sret(&mut self, ret: &Type) -> Option<i32> {
        // A class-returning callee uses an sret pointer: the caller allocates a
        // result temp and hands its address to the callee in r11.
        matches!(ret, Type::Named(_))
            .then(|| self.alloc(self.size_of(ret).max(1), self.align_of(ret)))
    }

    fn eval_arg_spill(&mut self, ty: &Type, arg: &Expr) -> Result<(), CodegenError> {
        if is_f64(ty) {
            self.gen_foperand(arg)?; // xmm0 = value
            self.asm.movq_r_from_xmm(RAX, 0);
        } else {
            self.gen_int_expr(arg, ty)?; // rax = value
        }
        self.asm.push_rax();
        Ok(())
    }

    fn place_args(
        &mut self,
        classes: &[crate::backend::ArgClass],
        extra: &[&Expr],
        varargs: bool,
        pos: Pos,
    ) -> Result<(), CodegenError> {
        let gpr = classes
            .iter()
            .filter(|c| matches!(c, crate::backend::ArgClass::Int(_)))
            .count();
        // Variadic: stage the trailing args into a frame buffer (8 bytes each, an F64
        // by its bit pattern), then push va_ptr and va_cnt for the next two int regs.
        // Always staged for a variadic callee (count 0 for none).
        let va = if varargs {
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
        for class in classes.iter().rev() {
            match *class {
                crate::backend::ArgClass::Float(i) => {
                    self.asm.pop_rax();
                    self.asm.movq_xmm_from_r(i as u8, RAX);
                }
                crate::backend::ArgClass::Int(i) => self.asm.pop_argreg(i),
            }
        }
        Ok(())
    }

    fn set_sret_reg(&mut self, slot: Option<i32>) {
        // Hand the result temp's address to the callee in r11, set last so nothing
        // clobbers it before the call.
        if let Some(off) = slot {
            self.asm.lea_local(off); // rax = &temp
            self.asm.mov_rr(R11, RAX);
        }
    }

    fn deliver_result(&mut self, _ret: &Type, sret: Option<i32>) {
        // For a non-aggregate return the result is already in rax/xmm0 by ABI; an
        // aggregate's rvalue *is* the temp's address.
        if let Some(off) = sret {
            self.asm.lea_local(off);
        }
    }

    fn frame_mark(&self) -> u32 {
        self.depth as u32 // depth is non-negative
    }

    fn frame_reset(&mut self, mark: u32) {
        self.depth = mark as i32; // reclaim the call's variadic buffer (max_depth preserved)
    }
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
