//! Compiler-recognized standard-library functions ("intrinsics").
//!
//! An intrinsic is an ordinary function defined in `lib/*.hc`. Sema resolves
//! calls to it and the interpreter runs its HolyC body. For a recognized name, a
//! native backend may emit a special lowering instead of a plain call: a single
//! instruction, a syscall, or bespoke runtime.
//!
//! This is the only seam for compiler-provided behaviour. Every algebraic or
//! OS-level operation is declared in a `lib/*.hc` file and recognized here. The
//! few things the compiler injects without any declaration are not intrinsics:
//! the command line `ArgC`/`ArgV` and a `...` function's `VargC`/`VargV` are
//! implicit globals and locals seeded by sema, not callable functions.
//!
//! The flavour is [`IntrinsicKind`]. An optimization intrinsic has a real HolyC
//! body that a backend may replace with a faster equivalent where the target
//! supports it (e.g. `Sqrt` → `fsqrt` / `sqrtsd`), falling back to the library
//! implementation otherwise. Conformance holds because both compute the same
//! value: the library `Sqrt` is correctly rounded and bit-identical to the
//! instruction. The interpreter never special-cases an optimization intrinsic; it
//! just runs the HolyC body.

/// How the backends may treat a recognized intrinsic call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IntrinsicKind {
    /// The library function has a real HolyC body, which is the portable
    /// implementation. Where the target supports it, a backend may emit a faster
    /// equivalent instruction in its place; otherwise it calls the body. Both
    /// produce the same value, so the interpreter, which always runs the body,
    /// stays conformant.
    Optimization,
    /// The library declaration is a prototype with no body. The function cannot be
    /// expressed in HolyC at all, because it bundles OS syscalls or the format
    /// machinery, so every backend and the interpreter must provide its lowering.
    /// These are the printf family, the heap, and the clock: real library
    /// functions you `#include`, but the compiler is their only implementation.
    Primitive,
}

/// Returns the intrinsic kind for `name`, or `None` if it is an ordinary
/// function.
pub fn kind(name: &str) -> Option<IntrinsicKind> {
    use IntrinsicKind::*;
    Some(match name {
        // Algebraic and rounding ops with a single-instruction equivalent on a
        // capable target. The HolyC fallback in `lib/math.hc` keeps the interpreter
        // and any backend lacking the instruction correct and in agreement.
        // Mappings: `Sqrt` → `fsqrt` / `sqrtsd`; `Fabs` → `fabs` / `andpd`; the
        // rounding family → the AArch64 `frint*` directed-rounding instructions.
        // x86 keeps the HolyC body for rounding, since `roundsd` needs SSE4.1
        // rather than baseline SSE2. The HolyC versions handle huge, inf, and NaN
        // inputs, so they match the instruction bit-for-bit.
        "Sqrt" | "Fabs" | "Floor" | "Ceil" | "Trunc" | "Round" | "RoundToEven" => Optimization,
        // The printf family. These are prototypes in `lib/fmt.hc`. The backends
        // render them via the shared `fmt` spec plus correctly-rounded bignum
        // floats; the interpreter renders via `crate::fmt`. Bare strings and the
        // `"fmt", args` comma form are lowered inline, not as calls to these, so
        // they need no include.
        "Print" | "StrPrint" | "CatPrint" | "MStrPrint" => Primitive,
        // The impure clock primitives, prototyped in `lib/time.hc`. They read the
        // OS clock or sleep, so they are non-reproducible: conformance is checked
        // by property, not by value.
        "UnixNS" | "NanoNS" | "Sleep" => Primitive,
        // The heap, prototyped in `lib/mem.hc`. A syscall or libc primitive: an
        // `mmap` bump allocator freestanding, or libc `malloc`/`free` hosted.
        // `HeapExtend` is the irreducible part of `realloc`; `MSize` reads a
        // block's tracked size.
        "MAlloc" | "Free" | "HeapExtend" | "MSize" => Primitive,
        // The raw fd I/O primitives, prototyped in `lib/io.hc` (files) and
        // `lib/net.hc` (sockets). Impure OS I/O, so non-reproducible like the
        // clock; raw syscalls freestanding, libc on Darwin.
        // `Read`/`Write`/`Close`/`Open`/`LSeek` are general fd ops shared by files
        // and sockets; `Socket`/`Connect` are the socket-specific pair. The libs
        // build `ReadFile`, `TcpConnect`, and so on, on top of these.
        "Socket" | "Connect" | "Open" | "LSeek" | "Read" | "Write" | "Close" => Primitive,
        // The standard-stream write primitive, prototyped in `lib/io.hc`.
        // `StdWrite(fd,…)` writes to stdout (fd 1) or stderr (fd 2) portably.
        // `Write` is a POSIX fd op with no Windows mapping; `StdWrite` instead
        // lowers per-target: the write syscall or libc on POSIX, and
        // `WriteFile(GetStdHandle(…))` on Windows. The interpreter routes fd 1 to
        // its captured output sink and fd 2 to real stderr. This is the sink
        // primitive the HolyC print machinery is built on.
        "StdWrite" => Primitive,
        // Filesystem mutation, prototyped in `lib/os.hc`. Impure, like the fd ops
        // above. Freestanding uses the aarch64 `*at` syscalls or x86-64 bare
        // syscalls; Darwin uses libc; the interpreter emulates over `std::fs`.
        // Each returns 0 on success, or `-errno`.
        "Remove" | "Rename" | "Mkdir" => Primitive,
        // Process control, prototyped in `lib/os.hc`. `Exit(code)` terminates the
        // process: freestanding `exit_group`, Darwin libc `exit`, Windows
        // `ExitProcess`, and the interpreter halts the run.
        // `Getpid`/`Getppid`/`Getuid` read process ids; impure, so property-tested.
        // All lower to a syscall or libc call.
        "Exit" | "Getpid" | "Getppid" | "Getuid" | "Getgid" => Primitive,
        // Working directory, prototyped in `lib/os.hc`. `Chdir(path)` wraps chdir
        // and `Getcwd(buf, size)` wraps getcwd, with its return normalised to
        // 0/-errno, over the syscall or libc. The interpreter uses `std::env`.
        // Impure, property-tested.
        "Chdir" | "Getcwd" => Primitive,
        // POSIX-style threads, prototyped in `lib/thread.hc`. Impure and
        // concurrent, so non-reproducible by value: libc
        // `pthread_create`/`pthread_join` on Darwin, raw `clone(2)` freestanding.
        // The interpreter runs the body synchronously.
        "Thread" | "Join" => Primitive,
        // Atomics, prototyped in `lib/sync.hc`. Lowered to the hardware atomic
        // instructions: `ldaxr`/`stlxr` loops, or `lock xadd`/`xchg`/`cmpxchg`.
        // The interpreter has synchronous threads and no contention, so it does a
        // plain read-modify-write. `Mutex` is pure HolyC on top of these.
        "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas" => Primitive,
        // Memory fence plus the kernel wait/wake behind the blocking `Mutex`:
        // `dmb` or `mfence`, and `futex(2)` freestanding or `__ulock_*` on Darwin.
        // No-ops in the synchronous interpreter.
        "AtomicFence" | "FutexWait" | "FutexWake" => Primitive,
        _ => return None,
    })
}

/// Whether `name` is a compiler-recognized intrinsic.
pub fn is_intrinsic(name: &str) -> bool {
    kind(name).is_some()
}

/// Whether `name` is a [`IntrinsicKind::Primitive`] intrinsic, one the backends
/// and interpreter must lower because it has only a library prototype and no
/// HolyC body. This predicate gates the bespoke call lowering in every backend and
/// the interpreter.
pub fn is_primitive(name: &str) -> bool {
    kind(name) == Some(IntrinsicKind::Primitive)
}
