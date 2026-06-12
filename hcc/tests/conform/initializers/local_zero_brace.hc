// Local class zeroed with empty brace init (all zero).

#include <stdio.hh>
class Big { I64 a; I64 b; I64 c; I64 d; };
Big b = {0};
"%d %d %d %d\n", b.a, b.b, b.c, b.d;
