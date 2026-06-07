#ifndef _FMT_HC
#define _FMT_HC
// fmt.hc — formatted output (the printf family).
//
// These are ordinary HolyC now, built on the private core in `<printf.hc>`, which
// walks the format string and renders each conversion into a sink. The sink is either
// a fd (`StdWrite`) or a buffer. The interpreter renders them via its own `crate::fmt`
// module, its independent conformance oracle, and never runs these bodies. So the
// native (compiled HolyC) output is diffed against an independent implementation.
// `#include <fmt.hc>` to call them.
//
// Note: a bare string statement (`"hi\n";`) lowers to a raw `StdWrite`, and the
// `"fmt", a, b` comma form desugars to a `Print(...)` call. The compiler auto-includes
// this file (and `<io.hc>`) when a program prints, so those need no explicit include.
// You only need this file to call `Print`/`StrPrint`/`CatPrint`/`MStrPrint` by name.
//
//   Print(fmt, ...)              — printf to stdout.
//   StrPrint(dst, fmt, ...)      — sprintf into `dst`; returns `dst`.
//   CatPrint(dst, fmt, ...)      — sprintf appended at `dst + StrLen(dst)`; returns `dst`.
//   MStrPrint(fmt, ...)          — asprintf into a fresh, growing heap buffer.

#include <printf.hc>

public U0 Print(U8 *fmt, ...)
{
  Pf p;
  p.dst = NULL;
  p.fd = STDOUT;
  p.len = 0;
  p.cap = 0;
  p.grow = 0;
  VFmt(&p, fmt, VargV, VargC);
}

public U8 *StrPrint(U8 *dst, U8 *fmt, ...)
{
  Pf p;
  p.dst = dst;
  p.fd = 0;
  p.len = 0;
  p.cap = 0;
  p.grow = 0;
  VFmt(&p, fmt, VargV, VargC);
  dst[p.len] = 0;
  return dst;
}

public U8 *CatPrint(U8 *dst, U8 *fmt, ...)
{
  Pf p;
  p.dst = dst;
  p.fd = 0;
  p.len = StrLen(dst); // append at the existing NUL
  p.cap = 0;
  p.grow = 0;
  VFmt(&p, fmt, VargV, VargC);
  dst[p.len] = 0;
  return dst;
}

public U8 *MStrPrint(U8 *fmt, ...)
{
  Pf p;
  p.cap = 64;
  p.dst = MAlloc(p.cap);
  p.fd = 0;
  p.len = 0;
  p.grow = 1;
  VFmt(&p, fmt, VargV, VargC);
  p.dst[p.len] = 0;
  return p.dst;
}

// `StrToF64` and its round-trip inverse `F64ToStr` now live in `<cstr.hc>`, next to the
// integer `StrToI64`/`I64ToStr` pair (and reachable from here transitively).

#endif
