// compare_fields.hc — compare fields of two class instances

#include <stdio.hh>
class Score { I64 pts; };
Score a; a.pts = 30;
Score b; b.pts = 50;
if (a.pts < b.pts) "b wins\n"; else "a wins\n";
