// A NULL-terminated list of pointers; iterate until NULL sentinel.

#include <stdio.hh>
I64 a = 1, b = 2, c = 3;
I64 *list[4];
list[0] = &a; list[1] = &b; list[2] = &c; list[3] = NULL;
I64 i;
for (i = 0; list[i] != NULL; i++)
  "%d ", *list[i];
"\n";
