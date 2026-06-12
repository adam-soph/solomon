//@ stdin: 1 2
//@ stdin: 30
//@ stdin: 4.5
//@ stdin: hello rest
// Streaming scanf: leftover input carries to the next call, conversions span lines.
#include <stdio.hh>
I64 a, b;
F64 f;
U8 w[32];
"%d\n", Scan("%d", &a);     // takes "1", leaves " 2"
"%d\n", a;
"%d\n", Scan("%d %d", &a, &b);  // "2" from the leftover + "30" from the next line
"%d %d\n", a, b;
"%d\n", Scan("%f", &f);
"%g\n", f;
"%d\n", Scan("%s", w);
"%s\n", w;
"%d\n", Scan("%s", w);      // "rest"
"%s\n", w;
"%d\n", Scan("%d", &a);     // end of input -> -1
