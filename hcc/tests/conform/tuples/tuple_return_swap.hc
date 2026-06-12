// tuple_return_swap.hc — function that swaps a tuple

#include <stdio.hh>
(I64, F64) Swap(I64 a, F64 b) { return a, b; }
a, b := Swap(5, 2.5);
"%d %.1f\n", a, b;
