#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  U8 *e;
  "%d %d %d %d %d\n",
    StrToI64Base("0xFF", 16, NULL),   // hex, explicit base
    StrToI64Base("0xff", 0, NULL),    // hex, auto-detected
    StrToI64Base("0755", 0, NULL),    // octal, auto-detected
    StrToI64Base("777", 8, NULL),     // octal, explicit
    StrToI64Base("-101", 2, NULL);    // binary, signed
  StrToI64Base("  42rest", 10, &e);   // endptr left just past the digits
  "endptr=[%s]\n", e;
  U8 *s = "zzz";                       // no digits: 0, endptr == start
  I64 v = StrToI64Base(s, 10, &e);
  "fail v=%d ateq=%d\n", v, e == s;
  "edge %d [%s]\n", StrToI64Base("0xZ", 0, &e), e; // "0x" w/o hex digit -> just "0"
  "compat %d %d %d\n", StrToI64("123"), StrToI64("  7x"), StrToI64("abc");
}
Main;
