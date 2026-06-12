// The portable program, which runs on all four targets. It exercises a *single* shared
// atomic counter under real threads, the mutex on its *uncontended* fast path
// (single-threaded, no kernel block), a fence, and the full width matrix.
#include <stdatomic.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
I64 acount = 0;
Mutex mu;
I64 mcount = 0;
I64 AWorker(I64 n) { I64 i; for (i = 0; i < 2000; i++) AtomicAdd(&acount, 1); return 0; }
U0 Main() {
  I64 h[4];
  I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&AWorker, i);
  for (i = 0; i < 4; i++) Join(h[i]);
  AtomicFence();
  MutexInit(&mu);
  for (i = 0; i < 8000; i++) { MutexLock(&mu); mcount++; MutexUnlock(&mu); }
  "acount=%d mcount=%d\n", acount, mcount;
  // Direct atomic semantics (I64).
  I64 x = 5;
  I64 sw = AtomicSwap(&x, 42);
  I64 c1 = AtomicCas(&x, 0, 7);
  I64 c2 = AtomicCas(&x, 42, 7);
  "x=%d sw=%d c1=%d c2=%d load=%d\n", x, sw, c1, c2, AtomicLoad(&x);
  // Width-directed atomics: U32 wraparound, U8 truncation, I16 sign-extension.
  U32 w = 0xFFFFFFFF; I64 nw = AtomicAdd(&w, 2);
  U8 b = 250; I64 nb = AtomicAdd(&b, 10);
  I16 s = -1; I64 cs = AtomicCas(&s, -1, -50);
  "w=%u nw=%d b=%d nb=%d cs=%d s=%d\n", w, nw, b, nb, cs, s;
}
Main;
