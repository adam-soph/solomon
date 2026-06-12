#include <stdio.hh>
#include <stdlib.hh>
#include <time.hh>
U0 Main() {
  DateTime dt = FromUnix(1700000000); // Tue 2023-11-14 22:13:20 UTC
  U8 b[128];
  Strftime(b, 128, "%Y-%m-%d %H:%M:%S", dt); "%s\n", b;
  Strftime(b, 128, "%a %A %b %B", dt); "%s\n", b;
  Strftime(b, 128, "%I:%M %p j=%j w=%w u=%u", dt); "%s\n", b;
  Strftime(b, 128, "%F %T %R %D %y %%", dt); "%s\n", b;
  Strftime(b, 128, "%c", dt); "%s\n", b;
  // truncation returns 0; a fitting one returns the length
  "trunc=%d ok=%d\n", Strftime(b, 5, "%Y-%m-%d", dt), Strftime(b, 8, "%H:%M", dt);
  DateTime e = FromUnix(0); Strftime(b, 128, "%a %F", e); "%s\n", b; // Thu epoch
}
Main;
