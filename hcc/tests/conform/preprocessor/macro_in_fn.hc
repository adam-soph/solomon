// macro_in_fn.hc — macros used inside function bodies

#include <stdio.hh>
#define INITIAL_VALUE 100
#define INCREMENT 5
I64 Accumulate(I64 n) {
  I64 acc = INITIAL_VALUE;
  I64 i;
  for (i = 0; i < n; i++) acc += INCREMENT;
  return acc;
}
"%d\n", Accumulate(3);
"%d\n", Accumulate(10);
