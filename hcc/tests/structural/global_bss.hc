
#include <stdio.hh>
#include <stdlib.hh>
I64 g = 5;
U0 Bump() { g++; }
Bump(); Bump();
"%d\n", g;
