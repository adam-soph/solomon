// varargs.hc — variadic functions via the implicit locals `VargC` and `VargV`. In a
// function declared with a trailing `...`, `VargC` is the number of variadic arguments
// and `VargV` is an `I64 *` of their raw 8-byte slots (so `VargV[i]` reads slot `i` as
// an integer; a stored F64 would be read back with `*(F64 *)&VargV[i]`). No #include
// needed — `VargC`/`VargV` are ambient inside a `...` function.

// Sum every integer argument, however many are passed.
I64 Sum(...)
{
  I64 total = 0, i;
  for (i = 0; i < VargC; i++)
    total += VargV[i];
  return total;
}

// The max of the arguments (or 0 when called with none).
I64 Max(...)
{
  if (VargC == 0) return 0;
  I64 best = VargV[0], i;
  for (i = 1; i < VargC; i++)
    if (VargV[i] > best) best = VargV[i];
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
