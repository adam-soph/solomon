// many_fields.hc — class with six I64 fields

#include <stdio.hh>
class Six { I64 a; I64 b; I64 c; I64 d; I64 e; I64 f; };
Six s; s.a = 1; s.b = 2; s.c = 3; s.d = 4; s.e = 5; s.f = 6;
"%d %d %d %d %d %d\n", s.a, s.b, s.c, s.d, s.e, s.f;
