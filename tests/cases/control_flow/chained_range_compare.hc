// Chained range comparisons: 0 <= i < n, a < b < c.
I64 x = 5;
if (0 <= x < 10)
  "in range\n";
else
  "out of range\n";

I64 a = 1, b = 5, c = 10;
if (a < b < c)
  "ordered\n";
else
  "not ordered\n";

// False case.
I64 y = 15;
if (0 <= y < 10)
  "in range\n";
else
  "out of range\n";
