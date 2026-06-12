// Mix of if/else and ternary for a clamp function.

#include <stdio.hh>
I64 Clamp(I64 v, I64 lo, I64 hi)
{
  return (v < lo) ? lo : (v > hi) ? hi : v;
}
"%d\n", Clamp(-5, 0, 10);
"%d\n", Clamp(5, 0, 10);
"%d\n", Clamp(15, 0, 10);
