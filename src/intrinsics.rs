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
        // The printf family `Print`/`StrPrint`/`CatPrint`/`MStrPrint` is **not** an
        // intrinsic: it is pure HolyC with real bodies in `lib/stdio.hc` (the `VFmt`
        // core over the format machinery, bottoming out at the `StdWrite` primitive).
        // Every target compiles and calls those bodies — the interpreter runs them,
        // and the backends synthesize `Print(fmt, …)` calls for bare strings and the
        // `"fmt", args` comma form. So they are ordinary functions, resolved by include.
        //
        // The impure clock primitives, prototyped in `lib/time.hc`. They read the
        // OS clock or sleep, so they are non-reproducible: conformance is checked
        // by property, not by value. `UnixNS` = CLOCK_REALTIME, `NanoNS` =
        // CLOCK_MONOTONIC, `CpuNS` = CLOCK_PROCESS_CPUTIME_ID (process CPU time).
        "UnixNS" | "NanoNS" | "CpuNS" | "Sleep" => Primitive,
        // The heap. `MAlloc`/`Free` are prototyped in the `lib/builtin.hc` prelude;
        // `HeapExtend`/`MSize` in `lib/stdlib.hc`. A
        // syscall or libc primitive: an `mmap` bump allocator freestanding, or libc
        // `malloc`/`free` hosted. `HeapExtend` is the irreducible part of `realloc`;
        // `MSize` reads a block's tracked size.
        "MAlloc" | "Free" | "HeapExtend" | "MSize" => Primitive,
        // The raw fd I/O primitives. `Open` is prototyped in `lib/fcntl.hc`,
        // `LSeek`/`Read`/`Write`/`Close` in `lib/unistd.hc`, `Socket`/`Connect` in
        // `lib/socket.hc`. Impure OS I/O, so non-reproducible like the clock; raw
        // syscalls freestanding, libc on Darwin. `Read`/`Write`/`Close`/`Open`/`LSeek`
        // are general fd ops shared by files and sockets; `Socket`/`Connect` are the
        // socket-specific pair. The libs build `ReadFile`, `TcpConnect`, and so on, on
        // top of these.
        "Socket" | "Connect" | "Open" | "LSeek" | "Read" | "Write" | "Close" => Primitive,
        // The standard-stream write primitive, prototyped in `lib/unistd.hc`.
        // `StdWrite(fd,…)` writes to stdout (fd 1) or stderr (fd 2) portably.
        // `Write` is a POSIX fd op with no Windows mapping; `StdWrite` instead
        // lowers per-target: the write syscall or libc on POSIX, and
        // `WriteFile(GetStdHandle(…))` on Windows. The interpreter routes fd 1 to
        // its captured output sink and fd 2 to real stderr. This is the sink
        // primitive the HolyC print machinery is built on.
        "StdWrite" => Primitive,
        // Filesystem mutation. `Remove`/`Rename` are prototyped in `lib/stdio.hc`
        // (C's `<stdio.h>`), `Mkdir` in `lib/unistd.hc`. Impure, like the fd ops
        // above. Freestanding uses the aarch64 `*at` syscalls or x86-64 bare
        // syscalls; Darwin uses libc; the interpreter emulates over `std::fs`.
        // Each returns 0 on success, or `-errno`.
        "Remove" | "Rename" | "Mkdir" => Primitive,
        // Process control and ids. `Exit` is prototyped in `lib/stdlib.hc`, the
        // `Getpid`/… family in `lib/unistd.hc`. `Exit(code)` terminates the process:
        // freestanding `exit_group`, Darwin libc `exit`, Windows `ExitProcess`, and
        // the interpreter halts the run. `Getpid`/`Getppid`/`Getuid` read process
        // ids; impure, so property-tested. All lower to a syscall or libc call.
        "Exit" | "Getpid" | "Getppid" | "Getuid" | "Getgid" => Primitive,
        // Working directory, prototyped in `lib/unistd.hc`. `Chdir(path)` wraps chdir
        // and `Getcwd(buf, size)` wraps getcwd, with its return normalised to
        // 0/-errno, over the syscall or libc. The interpreter uses `std::env`.
        // Impure, property-tested.
        "Chdir" | "Getcwd" => Primitive,
        // C11-style threads, prototyped in `lib/threads.hc`. Impure and
        // concurrent, so non-reproducible by value: libc
        // `pthread_create`/`pthread_join` on Darwin, raw `clone(2)` freestanding.
        // The interpreter runs the body synchronously.
        "Thread" | "Join" => Primitive,
        // Atomics, prototyped in `lib/stdatomic.hc`. Lowered to the hardware atomic
        // instructions: `ldaxr`/`stlxr` loops, or `lock xadd`/`xchg`/`cmpxchg`.
        // The interpreter has synchronous threads and no contention, so it does a
        // plain read-modify-write. `Mutex` is pure HolyC on top of these.
        "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas" => Primitive,
        // Memory fence plus the kernel wait/wake behind the blocking `Mutex`,
        // prototyped in `lib/stdatomic.hc`: `dmb` or `mfence`, and `futex(2)`
        // freestanding or `__ulock_*` on Darwin. No-ops in the synchronous interpreter.
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

/// Darwin → Linux `errno` remaps, as `(darwin, linux)` pairs, for the codes that can
/// reach a `-errno` return on the Darwin backend — the filesystem ops
/// (`Open`/`Remove`/`Rename`/`Mkdir`/`Chdir`/`Getcwd`). The fd and socket ops surface
/// a plain `-1` on Darwin today (not `-errno`), so the networking codes
/// (`ECONNREFUSED`, `ETIMEDOUT`, …) never flow through normalization and are omitted.
///
/// The overwhelming majority of file-domain codes already agree across the two systems
/// (`ENOENT` 2, `EACCES` 13, `EEXIST` 17, `EINVAL` 22, `EISDIR` 21, `ENOTDIR` 20,
/// `ENOSPC` 28, `EROFS` 30, `EMFILE` 24, `ENFILE` 23, `EFBIG` 27, …); only these few
/// differ. The values are the Linux-canonical ones the `lib/errno.hc` constants use.
/// Both the interpreter ([`darwin_to_linux_errno`]) and the AArch64 Darwin backend
/// (which emits a matching compare-chain) read this one table, so they cannot drift.
pub const DARWIN_TO_LINUX_ERRNO: &[(i64, i64)] = &[
    (35, 11), // EAGAIN / EWOULDBLOCK
    (11, 35), // EDEADLK
    (63, 36), // ENAMETOOLONG
    (62, 40), // ELOOP
    (66, 39), // ENOTEMPTY
];

/// Translate a positive Darwin `errno` to its Linux-canonical value, or return it
/// unchanged when the two systems already agree. See [`DARWIN_TO_LINUX_ERRNO`].
pub fn darwin_to_linux_errno(d: i64) -> i64 {
    DARWIN_TO_LINUX_ERRNO
        .iter()
        .find(|&&(darwin, _)| darwin == d)
        .map(|&(_, linux)| linux)
        .unwrap_or(d)
}
