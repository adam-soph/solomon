// && and || side-effect ordering.

#include <stdio.hh>
I64 g = 0;
I64 Bump(I64 v)
{
  g++;
  return v;
}

// All of: false && X, X is not evaluated.
g = 0;
I64 r = Bump(0) && Bump(99);
"g=%d r=%d\n", g, r;

// All of: true || X, X is not evaluated.
g = 0;
r = Bump(1) || Bump(99);
"g=%d r=%d\n", g, r;

// Both evaluated: true && true.
g = 0;
r = Bump(1) && Bump(1);
"g=%d r=%d\n", g, r;

// Both evaluated: false || false.
g = 0;
r = Bump(0) || Bump(0);
"g=%d r=%d\n", g, r;
