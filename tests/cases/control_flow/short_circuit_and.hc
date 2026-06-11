// Short-circuit && with side-effecting calls.
I64 g_count = 0;
I64 Inc(I64 ret)
{
  g_count++;
  return ret;
}

// Inc(0) is false, Inc(1) should NOT be called.
if (Inc(0) && Inc(1))
  "both\n";
else
  "short-circuited\n";
"count=%d\n", g_count;

// Inc(1) is true, Inc(1) SHOULD be called.
g_count = 0;
if (Inc(1) && Inc(1))
  "both true\n";
"count=%d\n", g_count;
