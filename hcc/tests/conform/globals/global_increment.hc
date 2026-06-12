// Global counter incremented across multiple function calls.

#include <stdio.hh>
#include <stdlib.hh>
I64 g_count;

U0 Bump() { g_count++; }

Bump(); Bump(); Bump();
"%d\n", g_count;
