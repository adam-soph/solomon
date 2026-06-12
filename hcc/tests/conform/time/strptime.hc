#include <stdio.hh>
#include <time.hh>
U8 b[64];
DateTime dt;
dt.year = 0; dt.month = 1; dt.day = 1; dt.hour = 0; dt.min = 0; dt.sec = 0; dt.wday = 0;
U8 *r = StrPTime("2026-06-11 14:30:05", "%Y-%m-%d %H:%M:%S", &dt);
"%s ok=%d\n", FmtISO(b, dt), r != NULL && *r == 0;
r = StrPTime("Thu Jun 11 14:30:05 2026", "%c", &dt);
"%s wday=%d ok=%d\n", FmtISO(b, dt), dt.wday, r != NULL;
r = StrPTime("07:30 PM", "%I:%M %p", &dt);
"hour=%d min=%d\n", dt.hour, dt.min;
r = StrPTime("12:00 AM", "%I:%M %p", &dt);
"midnight=%d\n", dt.hour;
r = StrPTime("3/14/26", "%D", &dt);
"%s\n", FmtISO(b, dt);
"%d\n", StrPTime("2026-13-01", "%Y-%m-%d", &dt) == NULL;  // month 13: mismatch
"%d\n", StrPTime("junk", "%Y", &dt) == NULL;
r = StrPTime("MARCH 5", "%B %d", &dt);   // names are case-insensitive
"month=%d day=%d\n", dt.month, dt.day;
