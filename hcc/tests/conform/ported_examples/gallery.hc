// gallery.hc — render a handful of numbers in many printf styles via StrPrint,
// showing the shared formatter: signed/unsigned, hex/octal, fixed-point,
// scientific (%e), and general (%g), with flags/width/precision. The integer
// columns use the truncated value; the float columns the original. Output is
// byte-identical in the interpreter and the native backend.


#include <stdio.hh>
#include <stdlib.hh>
#include <stdio.hh>   // Print / StrPrint

U0 Show(U8 *buf, U8 *label, F64 x) {
  I64 n = x;
  StrPrint(buf, "%-7s | %6d | %5x | %7o | %10.3f | %12.4e | %g\n",
           label, n, n, n, x, x, x);
}

U0 Main() {
  U8 *buf = MAlloc(256);
  StrPrint(buf, "%-7s | %6s | %5s | %7s | %10s | %12s | %s\n",
           "label", "dec", "hex", "oct", "fixed", "sci", "gen");
  "%s", buf;
  Show(buf, "small", 42.5);     "%s", buf;
  Show(buf, "big", 123456.789); "%s", buf;
  Show(buf, "tiny", 0.000123);  "%s", buf;
  Show(buf, "neg", -7.0);       "%s", buf;
  Free(buf);
}

Main;
