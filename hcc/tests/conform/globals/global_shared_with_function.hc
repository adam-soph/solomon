// A global written by a function and read at top level must stay shared storage (never
// promoted to an @entry SSA local): @entry and the function see the same variable.

#include <stdio.hh>
#include <stdlib.hh>
I64 counter = 0;
U0 Bump() { counter++; }
I64 i;
for (i = 0; i < 5; i++) Bump();
"counter=%d\n", counter;
Bump();
Bump();
"counter=%d\n", counter;
