//! x86-64 code-generation backend (Linux + Windows).
//!
//! The code generator itself is [`isel`], which walks the SSA [IR](crate::ir) and
//! hand-emits x86-64 (see that module). This file holds the shared, OS-agnostic pieces it
//! builds on: the [`Asm`] encoder (in [`asm`]), the register numbering, and the
//! [`OsTarget`] seam — the handful of points where the emitted program touches the
//! operating system (exit, page allocation, the stdout sink, file ops, the clock, the
//! command-line/env capture, and the container format). The Linux target ([`X64Linux`])
//! uses raw syscalls in a freestanding static ELF; the Windows target ([`X64Windows`])
//! calls `kernel32` imports from a self-contained PE. Each seam is a small instruction
//! sequence with a fixed register contract, so the backend drives it without knowing the
//! OS.

use crate::codegen::CodegenError;

mod asm;
mod isel;
mod linux;
mod windows;

use asm::Asm;
pub use linux::X64Linux;
pub use windows::X64Windows;

/// A file-descriptor primitive, dispatched through [`OsTarget::emit_fileop`].
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

    /// Read the process CPU time into `rax` as nanoseconds. `scratch` is at least 32
    /// bytes of BSS. Linux uses `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)`; Windows sums
    /// the kernel and user `FILETIME`s from `GetProcessTimes`.
    fn emit_cpu_ns(&mut self, asm: &mut Asm, scratch: i32);

    /// Suspend the thread for the nanosecond count in `rax`. Linux uses `nanosleep`;
    /// Windows uses `Sleep`, which has millisecond granularity.
    fn emit_sleep(&mut self, asm: &mut Asm, scratch: i32);

    /// Emit the entry preamble that captures the command line into the BSS slots
    /// `argc_off` and `argv_off`. The `argv_off` slot holds a pointer to the argv
    /// array. Runs just after the entry frame is set up, so the frame pointer `rbp`
    /// is valid, and only when the program uses `argc`/`argv`. Linux reads them off
    /// the initial stack; Windows builds them from `GetCommandLineA`.
    fn emit_capture_args(&mut self, asm: &mut Asm, argc_off: i32, argv_off: i32);

    /// Capture the environment pointer (`U8 **envp`) into the BSS slot `envp_off`.
    /// Runs just after the entry frame is set up, so `rbp` is valid, and only when
    /// the program references `envp`. Linux computes `&envp[0]` from the initial
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

// Register numbers for the generic encoders.
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R10: u8 = 10;
const R11: u8 = 11;
// r12–r14 are the callee-saved GPRs the IR backend promotes hot vregs into (with rbx).
// r15 is excluded: the Windows `OsTarget` seam uses it to save rsp around aligned calls.
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
const R15: u8 = 15;

/// Opcode bytes (before ModRM) for a width-aware load into rax (the reg field is rax;
/// the parametric forms in [`Asm`] OR a different register into the ModRM byte).
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

/// Round `n` up to a 16-byte boundary (stack-frame sizing).
fn align16(n: i32) -> i32 {
    (n + 15) & !15
}
