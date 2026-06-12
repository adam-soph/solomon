#include <stdio.hh>
#include <stdlib.hh>
#include <time.hh>
U0 Show(I64 s) {
  U8 b[32]; DateTime dt = FromUnix(s);
  "%s w%d L%d r%d\n", FmtISO(b, dt), dt.wday, IsLeap(dt.year), ToUnix(dt) == s;
}
U0 Main() { Show(0); Show(1717200000); Show(1000000000); Show(-86400); }
Main;
