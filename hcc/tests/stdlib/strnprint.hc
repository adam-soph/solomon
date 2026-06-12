#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  U8 buf[32];
  I64 r;
  r = StrNPrint(buf, 32, "%d-%s!", 42, "hello");  // fits: "42-hello!"
  "[%s] r=%d\n", buf, r;
  r = StrNPrint(buf, 5, "%d-%s!", 42, "hello");    // truncate to cap-1 = 4
  "[%s] r=%d\n", buf, r;
  r = StrNPrint(buf, 1, "abc");                    // only the NUL fits
  "[%s] r=%d\n", buf, r;
  StrCpy(buf, "ZZZ");
  r = StrNPrint(buf, 0, "abc");                    // nothing written, still counts
  "[%s] r=%d\n", buf, r;
}
Main;
