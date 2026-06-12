// union_sum_type.hc — tagged sum type (manual discriminated union)

#include <stdio.hh>
union Payload { I64 ival; F64 fval; };
class Var { I64 tag; union Payload data; };
Var a; a.tag = 0; a.data.ival = 42;
Var b; b.tag = 1; b.data.fval = 3.14;
if (a.tag == 0) "%d\n", a.data.ival;
if (b.tag == 1) "%f\n", b.data.fval;
