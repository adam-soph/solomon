#ifndef _TIME_HC
#define _TIME_HC
// time.hc — the impure OS clock primitives plus calendar math over them.
//
// The clock primitives `UnixNS`/`NanoNS`/`Sleep` are intrinsics: the prototypes are
// declared here, and the compiler lowers them to `clock_gettime`/`nanosleep` syscalls
// freestanding, libc on Darwin, and kernel32 on Windows. They read the OS clock or
// sleep, so they are the one non-reproducible group. Conformance for them is by
// property (monotonic across a `Sleep`, wall clock past 1970), never by
// interp-vs-native value comparison.
//
// The calendar layer (`FromUnix`/`ToUnix`/`FmtISO`/`Strftime`) is pure: a defined
// algorithm, Howard Hinnant's civil/days conversion, exact for any proleptic-Gregorian
// date. It computes the same bits everywhere; only `Now` reads the clock. Include with
// `#include <time.hc>`.

#include <stdio.hc>    // StrPrint/CatPrint for FmtISO

// --- clock primitives (intrinsics) -------------------------------------------

public I64 UnixNS();        // wall-clock nanoseconds since 1970 (CLOCK_REALTIME)
public I64 NanoNS();        // monotonic nanoseconds from an arbitrary origin (CLOCK_MONOTONIC)
public I64 CpuNS();         // process CPU-time nanoseconds (CLOCK_PROCESS_CPUTIME_ID)
public U0 Sleep(I64 ns);    // sleep for `ns` nanoseconds

// C `clock()`: process CPU time in `CLOCKS_PER_SEC` units (microseconds here, = CpuNS/1000).
// `Clock() / CLOCKS_PER_SEC` is CPU seconds. Like the other clocks it is impure.
#define CLOCKS_PER_SEC 1000000
public I64 Clock() { return CpuNS() / 1000; }

public class DateTime {
  I64 year, month, day;   // month 1..12, day 1..31
  I64 hour, min, sec;     // 0..23, 0..59, 0..59
  I64 wday;               // day of week, 0 = Sunday
}

// Broken-down UTC time from Unix-epoch seconds. Floor-divides, so pre-1970
// (negative) seconds work too.
public DateTime FromUnix(I64 secs)
{
  DateTime dt;
  I64 days = secs / 86400, tod = secs % 86400;
  if (tod < 0) { tod += 86400; days -= 1; }   // floor division
  dt.hour = tod / 3600;
  dt.min = (tod % 3600) / 60;
  dt.sec = tod % 60;
  dt.wday = ((days % 7) + 4 + 7) % 7;          // 1970-01-01 was a Thursday

  // civil_from_days: shift the epoch to 0000-03-01 so leap days fall at year end.
  I64 z = days + 719468;
  I64 era = (z >= 0 ? z : z - 146096) / 146097;
  I64 doe = z - era * 146097;                                  // [0, 146096]
  I64 yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
  I64 doy = doe - (365 * yoe + yoe / 4 - yoe / 100);           // [0, 365]
  I64 mp = (5 * doy + 2) / 153;                                // [0, 11]
  dt.day = doy - (153 * mp + 2) / 5 + 1;                       // [1, 31]
  dt.month = mp < 10 ? mp + 3 : mp - 9;                        // [1, 12]
  dt.year = yoe + era * 400 + (dt.month <= 2);
  return dt;
}

// The inverse: Unix-epoch seconds from a broken-down date (days_from_civil).
public I64 ToUnix(DateTime dt)
{
  I64 y = dt.year - (dt.month <= 2);
  I64 era = (y >= 0 ? y : y - 399) / 400;
  I64 yoe = y - era * 400;
  I64 mp = dt.month > 2 ? dt.month - 3 : dt.month + 9;
  I64 doy = (153 * mp + 2) / 5 + dt.day - 1;
  I64 doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  I64 days = era * 146097 + doe - 719468;
  return days * 86400 + dt.hour * 3600 + dt.min * 60 + dt.sec;
}

// Whether `year` is a Gregorian leap year.
public I64 IsLeap(I64 year)
{
  return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
}

// C `difftime`: the difference in seconds (here time is already I64 seconds, so this is
// just the subtraction, returned as F64 for C compatibility).
public F64 Difftime(I64 t1, I64 t0) { return t1 - t0; }

// Broken-down local time: `FromUnix` shifted by a caller-supplied UTC offset in seconds
// (e.g. -8*3600 for PST). There is no timezone database, so the offset is explicit —
// `FromUnix` itself is UTC (offset 0). Pure.
public DateTime Localtime(I64 secs, I64 tz_offset_sec)
{
  return FromUnix(secs + tz_offset_sec);
}

// "YYYY-MM-DD HH:MM:SS" into buf; returns buf.
public U8 *FmtISO(U8 *buf, DateTime dt)
{
  StrPrint(buf, "%04d-%02d-%02d %02d:%02d:%02d", dt.year, dt.month, dt.day,
           dt.hour, dt.min, dt.sec);
  return buf;
}

// Append NUL-terminated `src` to buf at *n (cap counts the final NUL). Returns 0 and
// stops on overflow, else 1 with *n advanced. (Private helper for Strftime.)
I64 SfPut(U8 *buf, I64 cap, I64 *n, U8 *src)
{
  I64 i = 0;
  while (src[i]) {
    if (*n + 1 >= cap) return 0; // no room for this byte plus the terminating NUL
    buf[*n] = src[i];
    (*n)++;
    i++;
  }
  return 1;
}

// `strftime`: render `dt` into `buf` (capacity `cap`, including the NUL) per `fmt`.
// Returns the byte count written (excluding the NUL), or 0 if it didn't fit (C semantics:
// `buf` is then indeterminate). Supported conversions:
//   %Y %y %C  year (full / 2-digit / century)     %m %d %e  month, day (0/space padded)
//   %H %I %M %S %p %P  time fields, AM/PM          %j        day of year (001-366)
//   %a %A %b %h %B  weekday / month names          %w %u     weekday number (0-6 / 1-7)
//   %F (%Y-%m-%d) %T %X (%H:%M:%S) %R (%H:%M)       %D %x (%m/%d/%y)  %c (asctime form)
//   %n %t %%  newline, tab, literal percent
// An unknown `%x` is emitted verbatim. Names are the C/POSIX ("C" locale) ones.
public I64 Strftime(U8 *buf, I64 cap, U8 *fmt, DateTime dt)
{
  if (cap <= 0) return 0;
  U8 *wda[7] = {"Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"};
  U8 *wdf[7] = {"Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday",
                "Saturday"};
  U8 *moa[12] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                 "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
  U8 *mof[12] = {"January", "February", "March", "April", "May", "June",
                 "July", "August", "September", "October", "November", "December"};
  I64 cum[12] = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334};
  I64 y2 = ((dt.year % 100) + 100) % 100; // 2-digit year, non-negative

  I64 n = 0, i = 0;
  U8 tmp[64];
  while (fmt[i]) {
    if (fmt[i] != '%') {
      if (n + 1 >= cap) return 0;
      buf[n++] = fmt[i++];
      continue;
    }
    i++; // past '%'
    U8 c = fmt[i];
    if (c) i++;
    U8 *out = tmp; // most conversions render into tmp; some point out at a literal/name
    if (c == 'Y') StrPrint(tmp, "%d", dt.year);
    else if (c == 'y') StrPrint(tmp, "%02d", y2);
    else if (c == 'C') StrPrint(tmp, "%02d", dt.year / 100);
    else if (c == 'm') StrPrint(tmp, "%02d", dt.month);
    else if (c == 'd') StrPrint(tmp, "%02d", dt.day);
    else if (c == 'e') StrPrint(tmp, "%2d", dt.day);
    else if (c == 'H') StrPrint(tmp, "%02d", dt.hour);
    else if (c == 'I') { I64 h = dt.hour % 12; if (h == 0) h = 12; StrPrint(tmp, "%02d", h); }
    else if (c == 'M') StrPrint(tmp, "%02d", dt.min);
    else if (c == 'S') StrPrint(tmp, "%02d", dt.sec);
    else if (c == 'p') out = dt.hour < 12 ? "AM" : "PM";
    else if (c == 'P') out = dt.hour < 12 ? "am" : "pm";
    else if (c == 'j') StrPrint(tmp, "%03d", cum[dt.month - 1] + dt.day
                                                 + (dt.month > 2 && IsLeap(dt.year)));
    else if (c == 'a') out = wda[dt.wday];
    else if (c == 'A') out = wdf[dt.wday];
    else if (c == 'b' || c == 'h') out = moa[dt.month - 1];
    else if (c == 'B') out = mof[dt.month - 1];
    else if (c == 'w') StrPrint(tmp, "%d", dt.wday);
    else if (c == 'u') StrPrint(tmp, "%d", dt.wday == 0 ? 7 : dt.wday);
    else if (c == 'F') StrPrint(tmp, "%04d-%02d-%02d", dt.year, dt.month, dt.day);
    else if (c == 'T' || c == 'X') StrPrint(tmp, "%02d:%02d:%02d", dt.hour, dt.min, dt.sec);
    else if (c == 'R') StrPrint(tmp, "%02d:%02d", dt.hour, dt.min);
    else if (c == 'D' || c == 'x') StrPrint(tmp, "%02d/%02d/%02d", dt.month, dt.day, y2);
    else if (c == 'c') StrPrint(tmp, "%s %s %2d %02d:%02d:%02d %d", wda[dt.wday],
                                moa[dt.month - 1], dt.day, dt.hour, dt.min, dt.sec, dt.year);
    else if (c == 'n') out = "\n";
    else if (c == 't') out = "\t";
    else if (c == '%') out = "%";
    else { tmp[0] = '%'; if (c) { tmp[1] = c; tmp[2] = 0; } else tmp[1] = 0; } // unknown: verbatim
    if (!SfPut(buf, cap, &n, out)) return 0;
  }
  buf[n] = 0; // SfPut/literal path keep n < cap, so this NUL fits
  return n;
}

// Current wall-clock UTC time, broken down. Impure (reads the clock).
public DateTime Now() { return FromUnix(UnixNS() / 1000000000); }

#endif
