// Spawn four threads computing Square(i) for i in 2..=5, join them, and print each
// result and the total. The stdout is deterministic regardless of thread interleaving.
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
I64 Square(I64 x) { return x * x; }
U0 Main() {
  I64 h[4];
  I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&Square, i + 2);
  I64 total = 0;
  for (i = 0; i < 4; i++) {
    I64 r = Join(h[i]);
    "t%d=%d\n", i, r;
    total += r;
  }
  "total=%d\n", total;
}
Main;
