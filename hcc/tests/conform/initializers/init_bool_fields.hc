// Class with Bool fields; designated init.

#include <stdio.hh>
class Flags { Bool a; Bool b; Bool c; };
Flags f = {.b = TRUE, .a = FALSE};
"%d %d %d\n", f.a, f.b, f.c;
