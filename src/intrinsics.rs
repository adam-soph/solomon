//! Compiler-recognized standard-library functions ("intrinsics").
//!
//! An intrinsic is an ordinary function **defined in `lib/*.hc`** — sema resolves
//! calls to it and the interpreter runs its HolyC body — for which a native backend
//! may emit a **special lowering** for the recognized name instead of a plain call:
//! a single instruction, a syscall, or bespoke runtime. This is the seam that lets
//! the builtin *registry* ([`crate::builtins`]) shrink to the handful of primitives
//! that can't be library functions at all (`ArgC`/`ArgV`/`VarArg*`) — everything
//! algebraic or OS-level lives in the library and is recognized here.
//!
//! The flavour is [`IntrinsicKind`]. An **optimization** has a real HolyC body the
//! backend may replace with a faster equivalent where the target supports it (e.g.
//! `Sqrt` → `fsqrt`/`sqrtsd`, falling back to the lib implementation otherwise);
//! conformance holds because both compute the same value (the lib `Sqrt` is
//! correctly rounded, bit-identical to the instruction). The interpreter never needs
//! to special-case an optimization intrinsic — it just runs the HolyC body.

/// How the backends may treat a recognized intrinsic call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IntrinsicKind {
    /// The lib function has a real HolyC body that is the portable implementation; a
    /// backend may emit a faster equivalent (an instruction) in its place where the
    /// target supports it, and otherwise just calls the body. Both produce the same
    /// value, so the interpreter (which always runs the body) stays conformant.
    Optimization,
    /// The lib declaration is a **prototype** (no body): the function can't be
    /// expressed in HolyC at all — it bundles OS syscalls or the format machinery —
    /// so every backend (and the interpreter) *must* provide its lowering. These are
    /// the printf family, the heap, and the clock: still real lib functions you
    /// `#include`, but the compiler is their only implementation.
    Primitive,
}

/// The intrinsic kind for `name`, or `None` if it is an ordinary function.
pub fn kind(name: &str) -> Option<IntrinsicKind> {
    use IntrinsicKind::*;
    Some(match name {
        // Algebraic / rounding ops with a single-instruction equivalent on a target
        // that has the HolyC fallback in `lib/math.hc` (so the interpreter and a
        // backend without the instruction stay correct and agree). `Sqrt` →
        // `fsqrt`/`sqrtsd`; `Fabs` → `fabs`/`andpd`; the rounding family → the AArch64
        // `frint*` directed-rounding instructions (x86 keeps the HolyC body — `roundsd`
        // needs SSE4.1, not baseline). The HolyC versions already handle huge/inf/NaN,
        // so they match the instruction bit-for-bit.
        "Sqrt" | "Fabs" | "Floor" | "Ceil" | "Trunc" | "Round" | "RoundToEven" => Optimization,
        // The printf family — `lib/fmt.hc` prototypes; the backends render via the
        // shared `fmt` spec + correctly-rounded bignum floats, the interpreter via
        // `crate::fmt`. (Bare strings and the `"fmt", args` comma form are lowered
        // inline, not as calls to these, so they need no include.)
        "Print" | "StrPrint" | "CatPrint" | "MStrPrint" => Primitive,
        // The impure clock primitives — `lib/time.hc` prototypes; read the OS clock
        // or sleep, so non-reproducible (conformance is by property, not value).
        "UnixNS" | "NanoNS" | "Sleep" => Primitive,
        // The heap — `lib/mem.hc` prototypes; a syscall/libc primitive (`mmap` bump
        // allocator freestanding, libc `malloc`/`free` hosted). `HeapExtend` is the
        // irreducible bit of `realloc`; `MSize` reads a block's tracked size.
        "MAlloc" | "Free" | "HeapExtend" | "MSize" => Primitive,
        // The raw fd I/O primitives — `lib/io.hc` prototypes (files) and `lib/net.hc`
        // (sockets); impure OS I/O (raw syscalls freestanding, libc on Darwin), so
        // non-reproducible like the clock. `Read`/`Write`/`Close`/`Open`/`LSeek` are
        // general fd ops shared by files and sockets; `Socket`/`Connect` are the
        // socket-specific pair. (The libs build `ReadFile`/`TcpConnect`/… on top.)
        "Socket" | "Connect" | "Open" | "LSeek" | "Read" | "Write" | "Close" => Primitive,
        // POSIX-style threads — `lib/thread.hc` prototypes; impure/concurrent (libc
        // `pthread_create`/`pthread_join` on Darwin, raw `clone(2)` freestanding), so
        // non-reproducible by value. The interpreter runs the body synchronously.
        "Thread" | "Join" => Primitive,
        // Atomics — `lib/sync.hc` prototypes; lowered to the hardware atomic
        // instructions (`ldaxr`/`stlxr` loops, `lock xadd`/`xchg`/`cmpxchg`). The
        // interpreter (synchronous threads, no contention) does a plain
        // read-modify-write. `Mutex` is pure HolyC on top.
        "AtomicLoad" | "AtomicStore" | "AtomicAdd" | "AtomicSwap" | "AtomicCas" => Primitive,
        // Memory fence + the kernel wait/wake behind the blocking `Mutex`: `dmb`/
        // `mfence`, and `futex(2)` (freestanding) / `__ulock_*` (Darwin). No-ops in the
        // synchronous interpreter.
        "AtomicFence" | "FutexWait" | "FutexWake" => Primitive,
        _ => return None,
    })
}

/// Whether `name` is a compiler-recognized intrinsic.
pub fn is_intrinsic(name: &str) -> bool {
    kind(name).is_some()
}

/// Whether `name` is a [`IntrinsicKind::Primitive`] intrinsic — one the backends and
/// interpreter must lower (it has only a lib prototype, no HolyC body). This is the
/// predicate that, alongside [`crate::builtins::is_builtin`], gates the bespoke
/// call lowering.
pub fn is_primitive(name: &str) -> bool {
    kind(name) == Some(IntrinsicKind::Primitive)
}
