// conditional_throw.hc — function throws conditionally

#include <stdio.hh>
#include <stdlib.hh>
U0 MaybeThrow(I64 x) {
  if (x % 2 == 0) throw(x);
  "odd %d\n", x;
}
I64 i;
for (i = 1; i <= 6; i++) {
  try {
    MaybeThrow(i);
  } catch {
    "even %d caught\n", Fs->except_ch;
  }
}
