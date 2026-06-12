#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  U8 *e;
  // strtoul: unsigned result, leading '-' wraps, full 64-bit range
  "%u %u\n", StrToU64Base("-1", 10, NULL),
             StrToU64Base("0xFFFFFFFFFFFFFFFF", 16, NULL);
  StrToU64Base("  255zzz", 0, &e);
  "uend=[%s]\n", e;
  // strtod: value + endptr
  "%.3f\n", StrToF64End("3.14159xyz", &e);
  "fend=[%s]\n", e;
  "%.1f\n", StrToF64End("  -2.5e2 ", &e);
  U8 *s = "nope";                         // no digits: 0.0, endptr == start
  F64 v = StrToF64End(s, &e);
  "fail %.1f ateq=%d\n", v, e == s;
}
Main;
