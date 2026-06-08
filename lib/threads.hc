#ifndef _THREADS_HC
#define _THREADS_HC
// threads.hc — C11 `<threads.h>`: spawn a function on a new thread and join it
// (`Thread`/`Join`), plus the blocking synchronization primitives built on atomics — a
// mutex (`Mutex`), a condition variable (`Cond`), and a reader/writer lock (`RwLock`).
//
// `Thread`/`Join` are intrinsics: the prototypes live here, and the compiler lowers them
// to libc `pthread_create`/`pthread_join` on Darwin, or to the raw `clone(2)` syscall
// with a hand-built child stack on the freestanding targets. Threads share the address
// space (globals and the heap), so they communicate through shared memory. The locks are
// pure-HolyC futex primitives over `<stdatomic.hc>` (Drepper "Futexes Are Tricky").
//
// Threading is impure and concurrent, so a program using it is not reproducible by
// value; conformance is by property. The interpreter (the oracle) runs each thread body
// synchronously at spawn time and returns the saved result on join, and `MutexLock`
// always acquires on the first try (never blocking). That matches a native run only for
// interleaving-independent, race-free work: have each thread write to its own slot or
// return its own value, then combine after joining. Include with `#include <threads.hc>`.

#include <stdatomic.hc>

// --- thread spawn / join (intrinsics) ----------------------------------------

// The thread entry point: takes one I64 argument, returns an I64 result. A bare
// function-pointer declarator names the type (the keyword-less form of a `typedef`).
I64 (*ThreadFn)(I64);

// Spawn `fn(arg)` on a new thread. Returns an opaque handle (pass to `Join`), or -1.
public I64 Thread(ThreadFn fn, I64 arg);

// Wait for the thread `handle` to finish; returns the value its function returned.
public I64 Join(I64 handle);

// --- mutex (a futex-backed 3-state lock; Drepper "Futexes Are Tricky") -------

public class Mutex { I64 state; };  // 0 = unlocked, 1 = locked, 2 = locked with waiters

public U0 MutexInit(Mutex *m)
{
  AtomicStore(&m->state, 0);
}

// Acquire the lock, blocking in the kernel (no busy spin) while it is held. The state
// tracks whether there may be waiters, so `MutexUnlock` only wakes when needed.
// Reentrant locking deadlocks, as for any plain mutex.
public U0 MutexLock(Mutex *m)
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
public I64 MutexTryLock(Mutex *m)
{
  return AtomicCas(&m->state, 0, 1) == 0;
}

public U0 MutexUnlock(Mutex *m)
{
  if (AtomicSwap(&m->state, 0) == 2)  // there were waiters: wake one
    FutexWake(&m->state, 1);
}

// --- condition variable (a futex over a sequence counter) --------------------

public class Cond { I64 seq; };

public U0 CondInit(Cond *c)
{
  AtomicStore(&c->seq, 0);
}

// Atomically release `m` and block until signaled, then re-acquire `m`. Must be called
// with `m` held. Always re-test your predicate in a loop, since a wakeup may be
// spurious. The `seq` snapshot taken before unlocking closes the lost-wakeup window: a
// signal in between bumps `seq`, so `FutexWait` returns at once instead of sleeping.
public U0 CondWait(Cond *c, Mutex *m)
{
  I64 seq = AtomicLoad(&c->seq);
  MutexUnlock(m);
  FutexWait(&c->seq, seq);
  MutexLock(m);
}

// Wake one waiter / all waiters. (Bump `seq` first so a waiter that hasn't slept yet
// also observes the change.)
public U0 CondSignal(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 1);
}

public U0 CondBroadcast(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 0x7FFFFFFF);  // wake all
}

// --- reader/writer lock (readers-preferred) ----------------------------------

public class RwLock { I64 state; };  // 0 = free, N>0 = N readers hold, -1 = a writer holds

public U0 RwLockInit(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
}

// Acquire a shared (read) lock. Any number of readers may hold it at once; it blocks
// only while a writer holds it.
public U0 RwLockRLock(RwLock *rw)
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

public U0 RwLockRUnlock(RwLock *rw)
{
  if (AtomicAdd(&rw->state, -1) == 0)  // the last reader left
    FutexWake(&rw->state, 1);          // let a waiting writer in
}

// Acquire the exclusive (write) lock. Blocks until there are no readers and no other
// writer.
public U0 RwLockWLock(RwLock *rw)
{
  while (AtomicCas(&rw->state, 0, -1) != 0)         // 0 -> -1 (exclusive)
    FutexWait(&rw->state, AtomicLoad(&rw->state));  // wait while not free
}

public U0 RwLockWUnlock(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
  FutexWake(&rw->state, 0x7FFFFFFF);  // wake all (waiting readers + a writer)
}

#endif
