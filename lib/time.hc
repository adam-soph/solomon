#ifndef _TIME_HC
#define _TIME_HC
// time.hc — implementation (interface in time.hh).

#include <time.hh>
#include <stdio.hh>
#include <string.hh>

// `Clock() / CLOCKS_PER_SEC` is CPU seconds. Like the other clocks it is impure.
public I64 Clock() { return CpuNS() / 1000; }

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
public I64 IsLeap(I64 year)
{
  return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
}
public F64 Difftime(I64 t1, I64 t0) { return t1 - t0; }
public DateTime Localtime(I64 secs, I64 tz_offset_sec)
{
  return FromUnix(secs + tz_offset_sec);
}
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
public I64 MkTime(DateTime *dt)
{
  // Carry out-of-range months into the year first (months are 1-based).
  I64 mo = dt->month - 1;
  I64 yc = mo / 12;
  mo = mo % 12;
  if (mo < 0) { mo += 12; yc--; }
  dt->month = mo + 1;
  dt->year += yc;
  // `ToUnix` is linear in day/hour/min/sec, so out-of-range values there simply
  // add/subtract whole days/hours/… — the round-trip normalizes them.
  I64 secs = ToUnix(*dt);
  *dt = FromUnix(secs);
  return secs;
}

// Parse digits (at most `wmax`) into *`out`; advances *`sp` and returns 1, or 0 if no
// digit is present. (Private helper for StrPTime.)
I64 TpNum(U8 **sp, I64 wmax, I64 *out)
{
  U8 *s = *sp;
  I64 v = 0, k = 0;
  while (k < wmax && s[k] >= '0' && s[k] <= '9') { v = v * 10 + (s[k] - '0'); k++; }
  if (k == 0) return 0;
  *out = v;
  *sp = s + k;
  return 1;
}

// Match *`sp` against a name table, case-insensitively, full names before
// abbreviations (so "March" isn't cut short at "Mar"). Advances *`sp` past the match
// and returns the index, or -1. (Private helper for StrPTime.)
I64 TpName(U8 **sp, U8 **full, U8 **abbr, I64 n)
{
  I64 i;
  for (i = 0; i < n; i++) {
    I64 fl = StrLen(full[i]);
    if (StrNCaseCmp(*sp, full[i], fl) == 0) { *sp += fl; return i; }
  }
  for (i = 0; i < n; i++) {
    I64 al = StrLen(abbr[i]);
    if (StrNCaseCmp(*sp, abbr[i], al) == 0) { *sp += al; return i; }
  }
  return -1;
}
public U8 *StrPTime(U8 *buf, U8 *fmt, DateTime *dt)
{
  U8 *wda[7] = {"Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"};
  U8 *wdf[7] = {"Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday",
                "Saturday"};
  U8 *moa[12] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                 "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
  U8 *mof[12] = {"January", "February", "March", "April", "May", "June",
                 "July", "August", "September", "October", "November", "December"};
  U8 *s = buf;
  I64 i = 0;
  I64 pm = -1, hour12 = -1; // %p/%I interplay, resolved at the end
  while (fmt[i]) {
    U8 fc = fmt[i];
    if ((fc >= 9 && fc <= 13) || fc == ' ') { // fmt whitespace: any input run
      i++;
      while ((*s >= 9 && *s <= 13) || *s == ' ') s++;
      continue;
    }
    if (fc != '%') { // an ordinary char must match
      if (*s != fc) return NULL;
      s++;
      i++;
      continue;
    }
    i++; // past '%'
    U8 c = fmt[i];
    if (c) i++;
    I64 v;
    if (c == 'n' || c == 't') {
      while ((*s >= 9 && *s <= 13) || *s == ' ') s++;
    } else if (c == '%') {
      if (*s != '%') return NULL;
      s++;
    } else if (c == 'Y') {
      if (!TpNum(&s, 9, &v)) return NULL;
      dt->year = v;
    } else if (c == 'y') {
      if (!TpNum(&s, 2, &v)) return NULL;
      dt->year = v >= 69 ? 1900 + v : 2000 + v;
    } else if (c == 'm') {
      if (!TpNum(&s, 2, &v) || v < 1 || v > 12) return NULL;
      dt->month = v;
    } else if (c == 'd' || c == 'e') {
      while (*s == ' ') s++; // %e is space-padded
      if (!TpNum(&s, 2, &v) || v < 1 || v > 31) return NULL;
      dt->day = v;
    } else if (c == 'H') {
      if (!TpNum(&s, 2, &v) || v > 23) return NULL;
      dt->hour = v;
    } else if (c == 'I') {
      if (!TpNum(&s, 2, &v) || v < 1 || v > 12) return NULL;
      hour12 = v;
      dt->hour = v; // refined by %p below, if present
    } else if (c == 'M') {
      if (!TpNum(&s, 2, &v) || v > 59) return NULL;
      dt->min = v;
    } else if (c == 'S') {
      if (!TpNum(&s, 2, &v) || v > 60) return NULL; // 60 admits a leap second, like C
      dt->sec = v;
    } else if (c == 'p') {
      if (StrNCaseCmp(s, "AM", 2) == 0) pm = 0;
      else if (StrNCaseCmp(s, "PM", 2) == 0) pm = 1;
      else return NULL;
      s += 2;
    } else if (c == 'a' || c == 'A') {
      v = TpName(&s, wdf, wda, 7);
      if (v < 0) return NULL;
      dt->wday = v;
    } else if (c == 'b' || c == 'h' || c == 'B') {
      v = TpName(&s, mof, moa, 12);
      if (v < 0) return NULL;
      dt->month = v + 1;
    } else if (c == 'F') {
      if (!(s = StrPTime(s, "%Y-%m-%d", dt))) return NULL;
    } else if (c == 'T' || c == 'X') {
      if (!(s = StrPTime(s, "%H:%M:%S", dt))) return NULL;
    } else if (c == 'R') {
      if (!(s = StrPTime(s, "%H:%M", dt))) return NULL;
    } else if (c == 'D' || c == 'x') {
      if (!(s = StrPTime(s, "%m/%d/%y", dt))) return NULL;
    } else if (c == 'c') {
      if (!(s = StrPTime(s, "%a %b %e %H:%M:%S %Y", dt))) return NULL;
    } else {
      return NULL; // unknown conversion
    }
  }
  if (pm >= 0 && hour12 >= 0) // %I and %p together: 12AM = 0, 12PM = 12
    dt->hour = hour12 % 12 + pm * 12;
  return s;
}
public U8 *AscTime(U8 *buf, DateTime dt)
{
  Strftime(buf, 26, "%c\n", dt);
  return buf;
}
public U8 *CTime(U8 *buf, I64 secs) { return AscTime(buf, FromUnix(secs)); }
public DateTime Now() { return FromUnix(UnixNS() / 1000000000); }

#endif
