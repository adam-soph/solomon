#ifndef _RAND_HC
#define _RAND_HC
// rand.hc — a small deterministic pseudo-random generator (splitmix64).
//
// Reproducible by construction: a defined algorithm over a 64-bit state, so it
// yields the same sequence on the interpreter and every backend. The seed defaults
// to 0; call `SeedRand` to start a different deterministic stream. (For
// *non*-reproducible randomness you would seed it from an OS entropy source —
// solomon has no such primitive yet.) Include with `#include <rand.hc>`.

U64 __rand_state = 0;

// Set the generator's seed; the next `RandU64` continues the stream from here.
U0 SeedRand(U64 seed) { __rand_state = seed; }

// The next pseudo-random 64-bit value (splitmix64).
U64 RandU64()
{
  __rand_state += 0x9e3779b97f4a7c15;
  U64 z = __rand_state;
  z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9;
  z = (z ^ (z >> 27)) * 0x94d049bb133111eb;
  return z ^ (z >> 31);
}

#endif
