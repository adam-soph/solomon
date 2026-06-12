// Short-circuit || with side-effecting calls.

#include <stdio.hh>
I64 g_count = 0;
I64 Inc(I64 ret)
{
  g_count++;
  return ret;
}

// Inc(1) is true, second should NOT be called.
if (Inc(1) || Inc(0))
  "or true\n";
"count=%d\n", g_count;

// Inc(0) is false, second SHOULD be called.
g_count = 0;
if (Inc(0) || Inc(1))
  "second true\n";
"count=%d\n", g_count;
