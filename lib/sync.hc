#ifndef _SYNC_HC
#define _SYNC_HC
// sync.hc — atomics and a mutex for sharing mutable state between threads.
//
// `AtomicLoad`/`AtomicStore`/`AtomicAdd`/`AtomicSwap`/`AtomicCas` are **intrinsics**
// (prototypes here; the compiler lowers them to the hardware atomic instructions —
// `ldaxr`/`stlxr` loops on AArch64, `lock`-prefixed `xadd`/`xchg`/`cmpxchg` on x86-64,
// with acquire/release ordering). They operate on a naturally-aligned `I64` in shared
// memory (a global or a heap/`MAlloc` slot — anything visible to the other thread).
// `AtomicFence` is a full barrier; `FutexWait`/`FutexWake` are the kernel wait/wake.
// `Mutex` is a **blocking** futex lock built on them in pure HolyC. Include with
// `#include <sync.hc>`.
//
// Conformance: the interpreter runs threads synchronously (see `thread.hc`), so it has
// no real contention — it performs each atomic as a plain read-modify-write, the
// fence/futex ops are no-ops, and `MutexLock` always acquires on the first try (never
// blocking). A native run with real threads gets the same answer for race-free use
// (atomic counters, mutex-guarded sections).

// --- atomic primitives (intrinsics) ------------------------------------------

I64 AtomicLoad(I64 *p);                            // atomic *p
U0  AtomicStore(I64 *p, I64 v);                    // atomic *p = v
I64 AtomicAdd(I64 *p, I64 delta);                  // *p += delta, returns the NEW value
I64 AtomicSwap(I64 *p, I64 v);                     // *p = v, returns the OLD value
I64 AtomicCas(I64 *p, I64 expected, I64 desired);  // CAS, returns the PREVIOUS value

// A full (sequentially-consistent) memory fence — `dmb ish` / `mfence`. Orders this
// thread's loads/stores around it; pairs with the acquire/release the atomics already
// carry. (A no-op in the synchronous interpreter.)
U0 AtomicFence();

// --- low-level futex (intrinsics) --------------------------------------------
//
// The kernel wait/wake used to build blocking primitives. They compare/sleep on the
// **low 32 bits** of the word at `addr` (Linux `futex(2)`; Darwin `__ulock_wait`/
// `__ulock_wake`). Most code wants `Mutex`, not these directly.

I64 FutexWait(I64 *addr, I64 expected);  // sleep while *addr == expected; wakes spuriously
I64 FutexWake(I64 *addr, I64 n);         // wake up to `n` waiters on `addr`

// --- atomic convenience -------------------------------------------------------

I64 AtomicInc(I64 *p) { return AtomicAdd(p, 1); }
I64 AtomicDec(I64 *p) { return AtomicAdd(p, -1); }

// --- mutex (a futex-backed 3-state lock; Drepper "Futexes Are Tricky") -------

class Mutex { I64 state; };  // 0 = unlocked, 1 = locked, 2 = locked with waiters

U0 MutexInit(Mutex *m)
{
  AtomicStore(&m->state, 0);
}

// Acquire the lock, **blocking** in the kernel (no busy spin) while it is held. The
// state tracks whether there may be waiters so `MutexUnlock` only wakes when needed.
// Reentrant locking deadlocks, as for any plain mutex.
U0 MutexLock(Mutex *m)
{
  I64 c = AtomicCas(&m->state, 0, 1);  // fast path: 0 -> 1
  if (c != 0) {
    if (c != 2) c = AtomicSwap(&m->state, 2);  // mark contended
    while (c != 0) {
      FutexWait(&m->state, 2);                 // sleep until woken
      c = AtomicSwap(&m->state, 2);            // re-acquire as contended
    }
  }
}

// Try to acquire without blocking. Returns 1 if the lock was taken, else 0.
I64 MutexTryLock(Mutex *m)
{
  return AtomicCas(&m->state, 0, 1) == 0;
}

U0 MutexUnlock(Mutex *m)
{
  if (AtomicSwap(&m->state, 0) == 2)  // there were waiters: wake one
    FutexWake(&m->state, 1);
}

// --- condition variable (a futex over a sequence counter) --------------------

class Cond { I64 seq; };

U0 CondInit(Cond *c)
{
  AtomicStore(&c->seq, 0);
}

// Atomically release `m` and block until signaled, then re-acquire `m`. Must be called
// with `m` held; always re-test your predicate in a loop (a wakeup may be spurious).
// The `seq` snapshot taken *before* unlocking closes the lost-wakeup window: a signal
// in between bumps `seq`, so `FutexWait` returns at once instead of sleeping.
U0 CondWait(Cond *c, Mutex *m)
{
  I64 seq = AtomicLoad(&c->seq);
  MutexUnlock(m);
  FutexWait(&c->seq, seq);
  MutexLock(m);
}

// Wake one waiter / all waiters. (Bump `seq` first so a waiter that hasn't slept yet
// also observes the change.)
U0 CondSignal(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 1);
}

U0 CondBroadcast(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 0x7FFFFFFF);  // wake all
}

// --- reader/writer lock (readers-preferred) ----------------------------------

class RwLock { I64 state; };  // 0 = free, N>0 = N readers hold, -1 = a writer holds

U0 RwLockInit(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
}

// Acquire a shared (read) lock — any number of readers may hold it at once, blocking
// only while a writer holds it.
U0 RwLockRLock(RwLock *rw)
{
  while (1) {
    I64 s = AtomicLoad(&rw->state);
    if (s >= 0) {
      if (AtomicCas(&rw->state, s, s + 1) == s) return;  // joined the readers
    } else {
      FutexWait(&rw->state, s);                          // a writer holds; wait
    }
  }
}

U0 RwLockRUnlock(RwLock *rw)
{
  if (AtomicAdd(&rw->state, -1) == 0)  // the last reader left
    FutexWake(&rw->state, 1);          // let a waiting writer in
}

// Acquire the exclusive (write) lock — blocks until no readers and no other writer.
U0 RwLockWLock(RwLock *rw)
{
  while (AtomicCas(&rw->state, 0, -1) != 0)         // 0 -> -1 (exclusive)
    FutexWait(&rw->state, AtomicLoad(&rw->state));  // wait while not free
}

U0 RwLockWUnlock(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
  FutexWake(&rw->state, 0x7FFFFFFF);  // wake all (waiting readers + a writer)
}

#endif
