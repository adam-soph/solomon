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
// The calendar layer (`FromUnix`/`ToUnix`/`FmtISO`) is pure: a defined algorithm,
// Howard Hinnant's civil/days conversion, exact for any proleptic-Gregorian date. It
// computes the same bits everywhere; only `Now` reads the clock. Include with
// `#include <time.hc>`.

#include <stdio.hc>    // StrPrint/CatPrint for FmtISO

// --- clock primitives (intrinsics) -------------------------------------------

public I64 UnixNS();        // wall-clock nanoseconds since 1970 (CLOCK_REALTIME)
public I64 NanoNS();        // monotonic nanoseconds from an arbitrary origin (CLOCK_MONOTONIC)
public U0 Sleep(I64 ns);    // sleep for `ns` nanoseconds

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

// "YYYY-MM-DD HH:MM:SS" into buf; returns buf.
public U8 *FmtISO(U8 *buf, DateTime dt)
{
  StrPrint(buf, "%04d-%02d-%02d %02d:%02d:%02d", dt.year, dt.month, dt.day,
           dt.hour, dt.min, dt.sec);
  return buf;
}

// Current wall-clock UTC time, broken down. Impure (reads the clock).
public DateTime Now() { return FromUnix(UnixNS() / 1000000000); }

#endif
