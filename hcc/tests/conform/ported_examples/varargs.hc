// varargs.hc — variadic functions via the implicit locals `argc` and `argv`. In a
// function declared with a trailing `...`, `argc` is the number of variadic arguments
// and `argv` is an `I64 *` of their raw 8-byte slots (so `argv[i]` reads slot `i` as
// an integer; a stored F64 would be read back with `*(F64 *)&argv[i]`). No #include
// needed — `argc`/`argv` are ambient inside a `...` function.

// Sum every integer argument, however many are passed.

#include <stdio.hh>
#include <stdlib.hh>
I64 Sum(...)
{
  I64 total = 0, i;
  for (i = 0; i < argc; i++)
    total += argv[i];
  return total;
}

// The max of the arguments (or 0 when called with none).
I64 Max(...)
{
  if (argc == 0) return 0;
  I64 best = argv[0], i;
  for (i = 1; i < argc; i++)
    if (argv[i] > best) best = argv[i];
  return best;
}

U0 Main()
{
  "sum()        = %d\n", Sum();
  "sum(1,2,3)   = %d\n", Sum(1, 2, 3);
  "sum(10..40)  = %d\n", Sum(10, 20, 30, 40);
  "max(4,9,2,7) = %d\n", Max(4, 9, 2, 7);
}

Main;
