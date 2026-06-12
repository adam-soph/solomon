// multi_return_bool.hc — second slot is a bool-like flag

#include <stdio.hh>
(I64, I64) SafeDiv(I64 a, I64 b) {
  if (b == 0) return 0, 0;
  return a/b, 1;
}
q, ok := SafeDiv(10, 3);
"%d ok=%d\n", q, ok;
q2, ok2 := SafeDiv(5, 0);
"%d ok=%d\n", q2, ok2;
