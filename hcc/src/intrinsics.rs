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
//! the command line `argc`/`argv` and a `...` function's `argc`/`argv` are
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
    /// expressed in HolyC at all, because it bundles an OS syscall (or a Win32 import),
    /// so every backend and the interpreter must provide its lowering. These are the
    /// `StdWrite` sink, the heap, the clock, fd/file I/O, sockets, fs mutation, process
    /// control, threads, atomics, and the Win32 imports: real library functions you
    /// `#include`, but the compiler is their only implementation. (The printf family is
    /// **not** here — it is pure HolyC, see below.)
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
        // socket-specific pair. The libs build their path/address helpers (`FileSize`,
        // `MakeSockaddr`, …) on top of these.
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
        // Process control and ids. `Exit`/`ExitRaw` are prototyped in
        // `lib/stdlib.hc`, the `Getpid`/… family in `lib/unistd.hc`. `Exit(code)`
        // terminates the process: freestanding `exit_group`, Darwin libc `exit`,
        // Windows `ExitProcess`, and the interpreter halts the run. `ExitRaw` (C
        // `_Exit`) is the same primitive under a different name — the difference is
        // entirely in lowering, which injects the `AtExit`-handler run only before a
        // call spelled `Exit`. `Getpid`/`Getppid`/`Getuid` read process ids; impure,
        // so property-tested. All lower to a syscall or libc call.
        "Exit" | "ExitRaw" | "Getpid" | "Getppid" | "Getuid" | "Getgid" => Primitive,
        // `System(cmd)` (C `system`), prototyped in `lib/stdlib.hc`: run `sh -c cmd`
        // and return its exit code. The interpreter uses `std::process`; Darwin libc
        // `system`; freestanding Linux a hand-emitted fork/execve/wait4; the Windows
        // backend rejects it (no process machinery there yet). Impure,
        // property-tested.
        "System" => Primitive,
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
        // The smaller thread ops, prototyped in `lib/threads.hc` (`Gettid` beside
        // `Getpid` in `lib/unistd.hc`): `ThreadYield` is `sched_yield` (kernel32
        // `SwitchToThread` on Windows); `ThreadDetach` is `pthread_detach` on Darwin
        // and a documented no-op freestanding (those stacks are never reclaimed);
        // `Gettid` reads the calling thread's OS id (kernel32 `GetCurrentThreadId`).
        "ThreadYield" | "ThreadDetach" | "Gettid" => Primitive,
        // `ThreadExit(ret)` (C11 `thrd_exit`) ends the calling thread with `ret` as
        // its `Join` value — Darwin `pthread_exit`, freestanding a store into the
        // thread control block (reached via the TLS register) + `exit(2)`; from the
        // main flow it exits the program with `ret`.
        "ThreadExit" => Primitive,
        // Atomics, prototyped in `lib/stdatomic.hc`. Lowered to the hardware atomic
        // instructions: `ldaxr`/`stlxr` loops, or `lock xadd`/`xchg`/`cmpxchg`.
        // The interpreter has synchronous threads and no contention, so it does a
        // plain read-modify-write. `Mutex` is pure HolyC on top of these.
        "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas" => Primitive,
        // Memory fence plus the kernel wait/wake behind the blocking `Mutex`,
        // prototyped in `lib/stdatomic.hc`: `dmb` or `mfence`, and `futex(2)`
        // freestanding or `__ulock_*` on Darwin. No-ops in the synchronous interpreter.
        "AtomicFence" | "FutexWait" | "FutexWake" => Primitive,
        // `FutexWait` with a caller-supplied nanosecond timeout, behind the timed
        // locks (`MutexTimedLock`/`CondTimedWait`). The interpreter returns a
        // timeout immediately (synchronous threads: nobody could wake it).
        "FutexWaitNs" => Primitive,
        // Win32 functions, prototyped in `lib/windows.hc` behind `#ifdef _WIN32`.
        // Each lowers to a kernel32 import on the `x86_64-pc-windows` backend (see
        // [`win_import`]); the other backends reject them, and the interpreter models
        // them over `std`. They are only ever in scope when compiling for Windows.
        n if win_import(n).is_some() => Primitive,
        _ => return None,
    })
}

/// The curated Win32 functions that `lib/windows.hc` exposes as import primitives,
/// returning the interned (`'static`) `(function name, DLL)`. This is the single source
/// of truth for the Windows-only surface: [`kind`] marks these `Primitive`, lowering
/// tags them `Prim::WinCall`, the Windows backend emits a direct import from the named
/// DLL + an MS-x64 call, and the interpreter models them.
///
/// Returning the `'static` literal (not the input `&str`) is what lets the IR carry a
/// `Copy` `&'static str` rather than an owned `String`.
pub fn win_import(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        // kernel32: file I/O + process. (Win32's `ReadFile`/`WriteFile` own those names;
        // a `CreateFileA` HANDLE also works with the portable `Read`/`Write`.)
        "CreateFileA" => ("CreateFileA", "kernel32.dll"),
        "ReadFile" => ("ReadFile", "kernel32.dll"),
        "WriteFile" => ("WriteFile", "kernel32.dll"),
        "CloseHandle" => ("CloseHandle", "kernel32.dll"),
        "SetFilePointerEx" => ("SetFilePointerEx", "kernel32.dll"),
        "GetFileSizeEx" => ("GetFileSizeEx", "kernel32.dll"),
        "GetLastError" => ("GetLastError", "kernel32.dll"),
        "GetCurrentProcessId" => ("GetCurrentProcessId", "kernel32.dll"),
        // Used by the portable `ThreadYield`/`Gettid` lowerings on the Windows
        // target (not `<windows.hc>` prototypes themselves).
        "SwitchToThread" => ("SwitchToThread", "kernel32.dll"),
        "GetCurrentThreadId" => ("GetCurrentThreadId", "kernel32.dll"),
        // advapi32: the registry — a genuinely Windows-only API with no POSIX analog.
        "RegCreateKeyExA" => ("RegCreateKeyExA", "advapi32.dll"),
        "RegSetValueExA" => ("RegSetValueExA", "advapi32.dll"),
        "RegQueryValueExA" => ("RegQueryValueExA", "advapi32.dll"),
        "RegCloseKey" => ("RegCloseKey", "advapi32.dll"),
        "RegDeleteKeyA" => ("RegDeleteKeyA", "advapi32.dll"),
        _ => return None,
    })
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
