// Global reset to a new value via a helper.

#include <stdio.hh>
#include <stdlib.hh>
I64 g_val = 7;

U0 Reset(I64 v) { g_val = v; }

"%d\n", g_val;
Reset(42);
"%d\n", g_val;
