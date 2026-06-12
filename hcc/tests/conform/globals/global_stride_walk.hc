// Walk a global array with a global pointer.

#include <stdio.hh>
I64 g_data[5] = {1, 3, 5, 7, 9};
I64 *g_cursor;

I64 NextVal() {
  I64 v = *g_cursor;
  g_cursor++;
  return v;
}

g_cursor = g_data;
I64 i;
for (i = 0; i < 5; i++) "%d ", NextVal();
"\n";
