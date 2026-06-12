#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%.17g %.17g %.17g\n", StrToF64("0.1"), StrToF64("0.2"), StrToF64("0.3");
  "%.17g %.17g\n", StrToF64("1e30"), StrToF64("123456789012345678");
  "%.17g\n", StrToF64("2.2250738585072014e-308");   // smallest normal
  "%.17g %.17g\n", StrToF64("1.7976931348623157e308"), StrToF64("6.022e23");
  "%.3f %.3f %.3f\n", StrToF64("3.14"), StrToF64("-2.5e2"), StrToF64("  6.0x");
  "%g %g %g %g\n", StrToF64("xyz"), StrToF64("1e309"), StrToF64("1e-330"), StrToF64("-0.0");
}
Main;
