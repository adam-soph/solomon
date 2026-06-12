// class_in_if.hc — class used in conditional branches

#include <stdio.hh>
class Status { I64 code; };
Status Ok() { Status s; s.code = 0; return s; }
Status Err() { Status s; s.code = -1; return s; }
Status a = Ok(); Status b = Err();
if (a.code == 0) "ok\n"; else "err\n";
if (b.code == 0) "ok\n"; else "err\n";
