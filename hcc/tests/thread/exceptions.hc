// Per-thread exception state: each worker throws and catches its own value, returning
// it through Join. Exception state (Fs) is thread-local, so concurrent throws never
// race — a shared global would corrupt the caught values. Deterministic regardless of
// interleaving (results are reported in join order).
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
I64 Worker(I64 id) {
  I64 got = -1;
  try { throw(id * 100); }
  catch { got = Fs->except_ch; }
  return got;
}
U0 Main() {
  I64 h[4];
  I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&Worker, i + 1);
  for (i = 0; i < 4; i++) "w%d=%d\n", i, Join(h[i]);
}
Main;
