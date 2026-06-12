#include <stdio.hh>
#include <time.hh>
U8 b[32];
DateTime dt;
dt.year = 1993; dt.month = 6; dt.day = 30;
dt.hour = 21; dt.min = 49; dt.sec = 8; dt.wday = 3;
"%s", AscTime(b, dt);
"%s", CTime(b, 0);
"%s", CTime(b, 86400 * 365);
