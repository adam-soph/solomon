// tuple_two_class_fields.hc — class with two tuple fields

#include <stdio.hh>
(I64, I64) DivMod(I64 a, I64 b) { return a/b, a%b; }
class Stats { (I64, I64) ab; (I64, I64) cd; };
Stats s;
s.ab = DivMod(10, 3);
s.cd = DivMod(20, 7);
"%d %d | %d %d\n", s.ab[0], s.ab[1], s.cd[0], s.cd[1];
