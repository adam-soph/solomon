// Global countdown: decremented until zero.

#include <stdio.hh>
#include <stdlib.hh>
I64 g_n = 5;

U0 Tick() { if (g_n > 0) g_n--; }

while (g_n > 0) { "%d ", g_n; Tick(); }
"\n";
