#ifndef _STDATOMIC_HH
#define _STDATOMIC_HH
// stdatomic.hh — C11 `<stdatomic.h>`: atomic operations on shared `I64` words, plus the
// low-level futex wait/wake used to build blocking primitives.
//
// `AtomicLoad`/`AtomicStore`/`AtomicAdd`/`AtomicSwap`/`AtomicCas` are intrinsics: the
// prototypes live here, and the compiler lowers them to the hardware atomic
// instructions — `ldaxr`/`stlxr` loops on AArch64, `lock`-prefixed
// `xadd`/`xchg`/`cmpxchg` on x86-64 — with acquire/release ordering. They operate on a
// naturally-aligned `I64` in shared memory: a global, or a heap/`MAlloc` slot, anything
// visible to the other thread. `AtomicFence` is a full barrier. `FutexWait`/`FutexWake`
// are the kernel wait/wake; most code wants `<threads.hc>`'s `Mutex`, not these directly.
// Include with `#include <stdatomic.hc>`.
//
// Conformance: the interpreter runs threads synchronously (see `<threads.hc>`), so it has
// no real contention. It performs each atomic as a plain read-modify-write, and the
// fence/futex ops are no-ops. A native run with real threads gets the same answer for
// race-free use (atomic counters, mutex-guarded sections).

// --- atomic primitives (intrinsics) ------------------------------------------

public I64 AtomicLoad(I64 *p);
public U0  AtomicStore(I64 *p, I64 v);
public I64 AtomicAdd(I64 *p, I64 delta);
public I64 AtomicSwap(I64 *p, I64 v);
public I64 AtomicCas(I64 *p, I64 expected, I64 desired);
public U0 AtomicFence();
public I64 FutexWait(I64 *addr, I64 expected);
public I64 FutexWake(I64 *addr, I64 n);
public I64 FutexWaitNs(I64 *addr, I64 expected, I64 ns);
public I64 AtomicInc(I64 *p);
public I64 AtomicDec(I64 *p);
public I64 AtomicAnd(I64 *p, I64 mask);
public I64 AtomicOr(I64 *p, I64 mask);
public I64 AtomicXor(I64 *p, I64 mask);
public I64 AtomicFlagTestAndSet(I64 *p);
public U0  AtomicFlagClear(I64 *p);

#endif
