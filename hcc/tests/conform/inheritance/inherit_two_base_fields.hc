// inherit_two_base_fields.hc — base with two fields, derived adds one

#include <stdio.hh>
class Base { I64 a; I64 b; };
class Sub : Base { I64 c; };
Sub s; s.a = 10; s.b = 20; s.c = 30;
Base *bp = (Base *)&s;
"%d %d %d\n", bp->a, bp->b, s.c;
