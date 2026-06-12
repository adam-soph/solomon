#include <stdio.hh>
#include <time.hh>
U8 b[64];
DateTime dt;
dt.year = 2026; dt.month = 1; dt.day = 32;   // Jan 32 -> Feb 1
dt.hour = 25; dt.min = 0; dt.sec = 0;        // hour 25 carries a day
I64 secs = MkTime(&dt);
"%s wday=%d\n", FmtISO(b, dt), dt.wday;
"%d\n", secs == ToUnix(dt);                  // normalized fields round-trip
dt.year = 2024; dt.month = 14; dt.day = 1;   // month 14 -> Feb next year
dt.hour = 0; dt.min = -1; dt.sec = 0;        // minute -1 borrows
MkTime(&dt);
"%s\n", FmtISO(b, dt);
dt.year = 2024; dt.month = 3; dt.day = 0;    // day 0 = last of Feb (leap year)
dt.hour = 12; dt.min = 30; dt.sec = 59;
MkTime(&dt);
"%s\n", FmtISO(b, dt);
