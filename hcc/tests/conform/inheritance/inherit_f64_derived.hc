// inherit_f64_derived.hc — derived class adds F64 field to integer base

#include <stdio.hh>
class IBase { I64 n; };
class FDerived : IBase { F64 val; };
FDerived fd; fd.n = 3; fd.val = 2.5;
"%d %f\n", fd.n, fd.val;
