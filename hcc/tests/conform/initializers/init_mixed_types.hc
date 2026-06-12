// Class with mixed int and F64 fields; positional init.

#include <stdio.hh>
class Mixed { I64 i; F64 f; I64 j; };
Mixed m = {1, 2.5, 3};
"%d %f %d\n", m.i, m.f, m.j;
