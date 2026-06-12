#ifndef _STDATOMIC_HC
#define _STDATOMIC_HC
// stdatomic.hc — implementation (interface in stdatomic.hh).

#include <stdatomic.hh>


// A full (sequentially-consistent) memory fence (`dmb ish` / `mfence`). Orders this
// thread's loads and stores around it, and pairs with the acquire/release the atomics
// already carry. (A no-op in the synchronous interpreter.)

// --- low-level futex (intrinsics) --------------------------------------------
//
// The kernel wait/wake used to build blocking primitives. They compare and sleep on
// the low 32 bits of the word at `addr` (Linux `futex(2)`; Darwin
// `__ulock_wait`/`__ulock_wake`). Most code wants `<threads.hc>`'s `Mutex`, not these.


// `FutexWait` with a caller-supplied timeout (nanoseconds) — what the timed locks in
// `<threads.hc>` are built on. Returns 0 when woken, or a negative value on
// timeout/value-mismatch; the exact code is target-flavoured (and the synchronous
// interpreter times out immediately, since no other thread could wake it), so
// re-check your predicate or deadline rather than the code.

// --- atomic convenience -------------------------------------------------------

public I64 AtomicInc(I64 *p) { return AtomicAdd(p, 1); }
public I64 AtomicDec(I64 *p) { return AtomicAdd(p, -1); }

// --- bitwise read-modify-write (CAS loops over `AtomicCas`) -------------------
//
// C11 `atomic_fetch_and`/`or`/`xor`. Each returns the OLD value, like C's `fetch_*`
// family (note the asymmetry with `AtomicAdd` above, which predates these and returns
// the NEW value). Pure HolyC retry loops, so they are atomic wherever `AtomicCas` is.

public I64 AtomicAnd(I64 *p, I64 mask)
{
  while (TRUE) {
    I64 old = AtomicLoad(p);
    if (AtomicCas(p, old, old & mask) == old) return old;
  }
}

public I64 AtomicOr(I64 *p, I64 mask)
{
  while (TRUE) {
    I64 old = AtomicLoad(p);
    if (AtomicCas(p, old, old | mask) == old) return old;
  }
}

public I64 AtomicXor(I64 *p, I64 mask)
{
  while (TRUE) {
    I64 old = AtomicLoad(p);
    if (AtomicCas(p, old, old ^ mask) == old) return old;
  }
}

// --- atomic flag (C11 `atomic_flag`) ------------------------------------------
//
// The flag is a plain `I64` word, 0 = clear. `AtomicFlagTestAndSet` sets it and
// returns whether it was already set (1) or this caller won it (0) — the C
// `atomic_flag_test_and_set` contract; `AtomicFlagClear` releases it.

public I64 AtomicFlagTestAndSet(I64 *p) { return AtomicSwap(p, 1) != 0; }
public U0  AtomicFlagClear(I64 *p)      { AtomicStore(p, 0); }


#endif
