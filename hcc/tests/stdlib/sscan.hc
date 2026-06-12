#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  I64 a, b; F64 f; U8 w[16]; U8 c;
  I64 n = SScan("  42 -7 3.14 hello X", "%d %d %f %s %c", &a, &b, &f, w, &c);
  "n=%d a=%d b=%d f=%.2f w=%s c=%c\n", n, a, b, f, w, c;
  I64 h, o, i1;                       // hex, octal, %i auto-base
  SScan("0xFF 075 0x10", "%x %o %i", &h, &o, &i1);
  "h=%d o=%d i1=%d\n", h, o, i1;
  F64 e; I64 z;                        // scientific float; %d then fails -> count 1
  I64 m = SScan("1.5e3 zzz", "%f %d", &e, &z);
  "m=%d e=%.1f\n", m, e;
  I64 keep, skip;                      // '*' suppresses assignment
  I64 k = SScan("1 2 3", "%d %*d %d", &keep, &skip);
  "k=%d keep=%d skip=%d\n", k, keep, skip;
  I64 only;                            // EOF before any match -> -1
  "r=%d\n", SScan("", "%d", &only);
}
Main;
