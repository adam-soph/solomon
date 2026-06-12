// discard_first.hc — discard first slot with _

#include <stdio.hh>
(I64, I64) MinMax(I64 a, I64 b) { return a<b?a:b, a>b?a:b; }
_, mx := MinMax(3, 8);
"max=%d\n", mx;
_, mx2 := MinMax(15, 2);
"max=%d\n", mx2;
