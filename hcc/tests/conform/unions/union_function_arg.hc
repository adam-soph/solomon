// union_function_arg.hc — pass union by value to a function

#include <stdio.hh>
union Duo { I64 a; I64 b; };
I64 GetA(Duo d) { return d.a; }
Duo d; d.a = 77;
"%d\n", GetA(d);
