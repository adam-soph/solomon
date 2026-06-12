// discard_middle.hc — discard the middle slot in a 3-tuple

#include <stdio.hh>
(I64, I64, I64) Trifecta(I64 x) { return x-1, x, x+1; }
lo, _, hi := Trifecta(10);
"%d %d\n", lo, hi;
