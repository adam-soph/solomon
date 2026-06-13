#ifndef _TIME_HH
#define _TIME_HH
// time.hh — the impure OS clock primitives plus calendar math over them.
//
// The clock primitives `UnixNS`/`NanoNS`/`CpuNS`/`Sleep` are intrinsics: the prototypes are
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


// --- clock primitives (intrinsics) -------------------------------------------

public I64 UnixNS();        // wall-clock nanoseconds since 1970 (CLOCK_REALTIME)
public I64 NanoNS();        // monotonic nanoseconds from an arbitrary origin (CLOCK_MONOTONIC)
public I64 CpuNS();         // process CPU-time nanoseconds (CLOCK_PROCESS_CPUTIME_ID)
public U0 Sleep(I64 ns);    // sleep for `ns` nanoseconds

// C `clock()`: process CPU time in `CLOCKS_PER_SEC` units (microseconds here, = CpuNS/1000).
#define CLOCKS_PER_SEC 1000000
public I64 Clock();

public class DateTime {
  I64 year, month, day;   // month 1..12, day 1..31
  I64 hour, min, sec;     // 0..23, 0..59, 0..59
  I64 wday;               // day of week, 0 = Sunday
}

// Broken-down UTC time from Unix-epoch seconds. Floor-divides, so pre-1970
// (negative) seconds work too.
public DateTime FromUnix(I64 secs);

// The inverse: Unix-epoch seconds from a broken-down date (days_from_civil).
public I64 ToUnix(DateTime dt);

// Whether `year` is a Gregorian leap year.
public I64 IsLeap(I64 year);

// C `difftime`: the difference in seconds (here time is already I64 seconds, so this is
// just the subtraction, returned as F64 for C compatibility).
public F64 Difftime(I64 t1, I64 t0);

// Broken-down local time: `FromUnix` shifted by a caller-supplied UTC offset in seconds
// (e.g. -8*3600 for PST). There is no timezone database, so the offset is explicit —
// `FromUnix` itself is UTC (offset 0). Pure.
public DateTime Localtime(I64 secs, I64 tz_offset_sec);

// "YYYY-MM-DD HH:MM:SS" into buf; returns buf.
public U8 *FmtISO(U8 *buf, DateTime dt);

// `strftime`: render `dt` into `buf` (capacity `cap`, including the NUL) per `fmt`.
// Returns the byte count written (excluding the NUL), or 0 if it didn't fit (C semantics:
// `buf` is then indeterminate). Supported conversions:
//   %Y %y %C  year (full / 2-digit / century)     %m %d %e  month, day (0/space padded)
//   %H %I %M %S %p %P  time fields, AM/PM          %j        day of year (001-366)
//   %a %A %b %h %B  weekday / month names          %w %u     weekday number (0-6 / 1-7)
//   %F (%Y-%m-%d) %T %X (%H:%M:%S) %R (%H:%M)       %D %x (%m/%d/%y)  %c (asctime form)
//   %n %t %%  newline, tab, literal percent
// An unknown `%x` is emitted verbatim. Names are the C/POSIX ("C" locale) ones.
public I64 Strftime(U8 *buf, I64 cap, U8 *fmt, DateTime dt);

// C `mktime`, the normalizing inverse: like `ToUnix`, but the fields may be out of
// range (Jan 32, month 13, minute -5, …) — each carries into the next-larger unit —
// and `dt` is rewritten with the normalized fields (`wday` filled in). Returns the
// epoch seconds. (UTC, like everything here: there is no timezone database, so this
// is C's `timegm` with `mktime`'s normalization.)
public I64 MkTime(DateTime *dt);

// `strptime`: parse `buf` per `fmt`, writing the fields that appear into `dt` (others
// are left untouched — initialize `dt` first, e.g. from `Now` or zeroed). Returns the
// position in `buf` just past the parsed input, or NULL on mismatch. Supported
// conversions (the parseable subset of `Strftime`'s):
//   %Y %y     year (full / 2-digit, POSIX pivot: 69-99 → 19xx, 00-68 → 20xx)
//   %m %d %e  month, day                          %H %I %M %S  time fields (%I + %p)
//   %p        AM/PM (with %I)                     %a %A %b %h %B  weekday/month names
//   %F %T %X %R %D %x %c  the composite forms     %n %t %%  whitespace, literal '%'
// Whitespace in `fmt` matches any run of input whitespace; numeric fields are
// range-checked like C. (UTC; there is no %Z/%z timezone handling.)
public U8 *StrPTime(U8 *buf, U8 *fmt, DateTime *dt);

// C `asctime`: render `dt` in the fixed "Wed Jun 30 21:49:08 1993\n" form (the
// `Strftime` "%c" form plus the trailing newline). `buf` needs at least 26 bytes,
// like C. Returns `buf`.
public U8 *AscTime(U8 *buf, DateTime dt);

// C `ctime`: `AscTime` of the broken-down epoch seconds. (UTC — everything here is;
// C's ctime is local-time, but there is no timezone database.)
public U8 *CTime(U8 *buf, I64 secs);

// Current wall-clock UTC time, broken down. Impure (reads the clock).
public DateTime Now();

#endif
