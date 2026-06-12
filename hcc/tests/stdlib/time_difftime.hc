#include <stdio.hh>
#include <stdlib.hh>
#include <time.hh>
U0 Main() {
  "%.1f\n", Difftime(1000, 250);             // pure: 750.0 seconds
  DateTime l = Localtime(1700000000, -8 * 3600); // UTC 22:13:20 -> PST 14:13:20
  U8 b[64]; FmtISO(b, l); "%s\n", b;
  // CpuNS/Clock are impure -> property only: non-negative and non-decreasing
  I64 a = CpuNS(), s = 0, i;
  for (i = 0; i < 1000000; i++) s += i;
  I64 c = CpuNS();
  "%d %d %d\n", a >= 0, c >= a, Clock() >= 0;
  "%d\n", CLOCKS_PER_SEC;
}
Main;
