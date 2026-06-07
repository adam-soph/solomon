#ifndef _ATOMIC_HC
#define _ATOMIC_HC
// atomic.hc — C11 `<stdatomic.h>`: atomic operations on shared `I64` words, plus the
// low-level futex wait/wake used to build blocking primitives.
//
// `AtomicLoad`/`AtomicStore`/`AtomicAdd`/`AtomicSwap`/`AtomicCas` are intrinsics: the
// prototypes live here, and the compiler lowers them to the hardware atomic
// instructions — `ldaxr`/`stlxr` loops on AArch64, `lock`-prefixed
// `xadd`/`xchg`/`cmpxchg` on x86-64 — with acquire/release ordering. They operate on a
// naturally-aligned `I64` in shared memory: a global, or a heap/`MAlloc` slot, anything
// visible to the other thread. `AtomicFence` is a full barrier. `FutexWait`/`FutexWake`
// are the kernel wait/wake; most code wants `<threads.hc>`'s `Mutex`, not these directly.
// Include with `#include <atomic.hc>`.
//
// Conformance: the interpreter runs threads synchronously (see `<threads.hc>`), so it has
// no real contention. It performs each atomic as a plain read-modify-write, and the
// fence/futex ops are no-ops. A native run with real threads gets the same answer for
// race-free use (atomic counters, mutex-guarded sections).

// --- atomic primitives (intrinsics) ------------------------------------------

public I64 AtomicLoad(I64 *p);                            // atomic *p
public U0  AtomicStore(I64 *p, I64 v);                    // atomic *p = v
public I64 AtomicAdd(I64 *p, I64 delta);                  // *p += delta, returns the NEW value
public I64 AtomicSwap(I64 *p, I64 v);                     // *p = v, returns the OLD value
public I64 AtomicCas(I64 *p, I64 expected, I64 desired);  // CAS, returns the PREVIOUS value

// A full (sequentially-consistent) memory fence (`dmb ish` / `mfence`). Orders this
// thread's loads and stores around it, and pairs with the acquire/release the atomics
// already carry. (A no-op in the synchronous interpreter.)
public U0 AtomicFence();

// --- low-level futex (intrinsics) --------------------------------------------
//
// The kernel wait/wake used to build blocking primitives. They compare and sleep on
// the low 32 bits of the word at `addr` (Linux `futex(2)`; Darwin
// `__ulock_wait`/`__ulock_wake`). Most code wants `<threads.hc>`'s `Mutex`, not these.

public I64 FutexWait(I64 *addr, I64 expected);  // sleep while *addr == expected; wakes spuriously
public I64 FutexWake(I64 *addr, I64 n);         // wake up to `n` waiters on `addr`

// --- atomic convenience -------------------------------------------------------

public I64 AtomicInc(I64 *p) { return AtomicAdd(p, 1); }
public I64 AtomicDec(I64 *p) { return AtomicAdd(p, -1); }

#endif
