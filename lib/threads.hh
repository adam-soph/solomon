#ifndef _THREADS_HH
#define _THREADS_HH
// threads.hh — C11 `<threads.h>`: spawn a function on a new thread and join it
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


// --- thread spawn / join (intrinsics) ----------------------------------------

// The thread entry point: takes one I64 argument, returns an I64 result. A bare
// function-pointer declarator names the type (the keyword-less form of a `typedef`).
I64 (*ThreadFn)(I64);

// Spawn `fn(arg)` on a new thread. Returns an opaque handle (pass to `Join`), or -1.
public I64 Thread(ThreadFn fn, I64 arg);

// Wait for the thread `handle` to finish; returns the value its function returned.
public I64 Join(I64 handle);

// Yield the processor to another runnable thread (C11 `thrd_yield`). Returns 0.
public I64 ThreadYield();

// Terminate the calling thread with result `ret` (C11 `thrd_exit`): a later
// `Join(handle)` returns `ret` exactly as if the thread function had returned it.
// Does not return. Called from the main flow (outside any thread) it terminates the
// whole program with `ret` as the exit status.
public U0 ThreadExit(I64 ret);

// Mark `handle` as never-joined (C11 `thrd_detach`): its resources are released when
// it finishes, and passing it to `Join` afterwards is undefined. On the freestanding
// targets this is a documented no-op — a clone(2) thread's 128 KiB stack region is
// never reclaimed (detached or not), so detach only forfeits the join. Returns 0.
public I64 ThreadDetach(I64 handle);

// --- mutex (a futex-backed 3-state lock; Drepper "Futexes Are Tricky") -------

public class Mutex { I64 state; };  // 0 = unlocked, 1 = locked, 2 = locked with waiters

public U0 MutexInit(Mutex *m);

// Acquire the lock, blocking in the kernel (no busy spin) while it is held. The state
// tracks whether there may be waiters, so `MutexUnlock` only wakes when needed.
// Reentrant locking deadlocks, as for any plain mutex.
public U0 MutexLock(Mutex *m);

// Try to acquire without blocking. Returns 1 if the lock was taken, else 0.
public I64 MutexTryLock(Mutex *m);

public U0 MutexUnlock(Mutex *m);

// Acquire within `ns` nanoseconds (C11 `mtx_timedlock`, with a relative timeout).
// Returns 1 if the lock was taken, 0 on timeout. Deadline-driven over the monotonic
// clock, so a spurious futex wake just re-checks; in the synchronous interpreter a
// contended call (necessarily a self-deadlock) times out cleanly.
public I64 MutexTimedLock(Mutex *m, I64 ns);

// --- condition variable (a futex over a sequence counter) --------------------

public class Cond { I64 seq; };

public U0 CondInit(Cond *c);

// Atomically release `m` and block until signaled, then re-acquire `m`. Must be called
// with `m` held. Always re-test your predicate in a loop, since a wakeup may be
// spurious. The `seq` snapshot taken before unlocking closes the lost-wakeup window: a
// signal in between bumps `seq`, so `FutexWait` returns at once instead of sleeping.
public U0 CondWait(Cond *c, Mutex *m);

// `CondWait` with a relative timeout in nanoseconds (C11 `cnd_timedwait`). Returns 1
// if a signal arrived (the sequence advanced — though as with any condition variable,
// re-test your predicate), 0 on timeout. The mutex is re-acquired either way.
public I64 CondTimedWait(Cond *c, Mutex *m, I64 ns);

// Wake one waiter / all waiters. (Bump `seq` first so a waiter that hasn't slept yet
// also observes the change.)
public U0 CondSignal(Cond *c);

public U0 CondBroadcast(Cond *c);

// --- reader/writer lock (readers-preferred) ----------------------------------

public class RwLock { I64 state; };  // 0 = free, N>0 = N readers hold, -1 = a writer holds

public U0 RwLockInit(RwLock *rw);

// Acquire a shared (read) lock. Any number of readers may hold it at once; it blocks
// only while a writer holds it.
public U0 RwLockRLock(RwLock *rw);

public U0 RwLockRUnlock(RwLock *rw);

// Acquire the exclusive (write) lock. Blocks until there are no readers and no other
// writer.
public U0 RwLockWLock(RwLock *rw);

public U0 RwLockWUnlock(RwLock *rw);

// --- one-time initialization (C11 `call_once`) --------------------------------

public class Once { I64 state; };  // 0 = not run, 1 = running, 2 = done

public U0 OnceInit(Once *o);

// Run `fn` exactly once across all threads sharing `o` (zero-initialized, or via
// `OnceInit`). The first caller runs `fn`; concurrent callers block in the kernel
// until it completes; every later call returns at once. Like any `call_once`, `fn`
// must not call `CallOnce` on the same `Once` (deadlock).
public U0 CallOnce(Once *o, U0 (*fn)());

// Allocate a fresh key, distinct from every other key in the program.
public I64 TssCreate();

// Set this thread's value for `key` (creating the slot on first use).
public U0 TssSet(I64 key, I64 val);

// This thread's value for `key`, or 0 if it has never been set here.
public I64 TssGet(I64 key);

#endif
