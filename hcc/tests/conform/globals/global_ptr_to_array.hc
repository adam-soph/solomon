// Global pointer initialized to point at a global array.

#include <stdio.hh>
I64 g_arr[4] = {100, 200, 300, 400};
I64 *g_ptr;

g_ptr = g_arr;
"%d %d\n", g_ptr[0], g_ptr[2];
g_ptr = g_arr + 1;
"%d\n", *g_ptr;
