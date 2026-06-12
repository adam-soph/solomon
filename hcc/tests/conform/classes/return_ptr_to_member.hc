// return_ptr_to_member.hc — return pointer to a global class member

#include <stdio.hh>
class Pair { I64 a; I64 b; };
Pair g; g.a = 11; g.b = 22;
I64 *GetA() { return &g.a; }
I64 *GetB() { return &g.b; }
*GetA() = 55;
"%d %d\n", g.a, g.b;
