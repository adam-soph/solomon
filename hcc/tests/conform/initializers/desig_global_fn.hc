// Global designated initializer read back through a function.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
Pt g_pt = {.x = 5, .y = 9};

I64 Sum() { return g_pt.x + g_pt.y; }

"%d\n", Sum();
