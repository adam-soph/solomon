// Global F64 accumulated across calls.

#include <stdio.hh>
#include <stdlib.hh>
F64 g_sum;

U0 Add(F64 x) { g_sum += x; }

Add(1.5); Add(2.5); Add(3.0);
"%f\n", g_sum;
