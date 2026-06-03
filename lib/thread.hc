#ifndef _THREAD_HC
#define _THREAD_HC
// thread.hc — POSIX-style threads: spawn a function on a new thread and join it.
//
// `Thread`/`Join` are **intrinsics** (prototypes here; the compiler lowers them to
// libc `pthread_create`/`pthread_join` on Darwin and the raw `clone(2)` syscall with
// a hand-built child stack on the freestanding targets). Threads share the address
// space (globals + the heap), so they communicate through shared memory.
//
// Threading is impure and concurrent, so a program using it is *not* reproducible by
// value — conformance is by **property**. The interpreter (the oracle) runs each
// thread body **synchronously at spawn time** and returns the saved result on join,
// which matches a native run only for **interleaving-independent** work: have each
// thread write to its *own* slot / return its own value and combine the results after
// joining (don't race on a shared counter). Include with `#include <thread.hc>`.

// The thread entry point: takes one I64 argument, returns an I64 result.
typedef I64 (*ThreadFn)(I64);

// Spawn `fn(arg)` on a new thread. Returns an opaque handle (pass to `Join`), or -1.
I64 Thread(ThreadFn fn, I64 arg);

// Wait for the thread `handle` to finish; returns the value its function returned.
I64 Join(I64 handle);

#endif
