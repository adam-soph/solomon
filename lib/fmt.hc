#ifndef _FMT_HC
#define _FMT_HC
// fmt.hc — formatted output (the printf family).
//
// These are **primitive intrinsics**: declared here as prototypes, but the compiler
// is their implementation — they bundle the shared `%[flags][width][.prec]conv`
// format machinery and the correctly-rounded bignum float formatting, which the
// backends emit as runtime (and the interpreter renders directly), so they can't be
// ordinary HolyC. `#include <fmt.hc>` to call them.
//
// Note: a bare string statement (`"hi\n";`) and the `"fmt", a, b` comma form are
// lowered inline by the compiler — they are *not* calls to `Print`, so they need no
// include. You only need this file to call `Print`/`StrPrint`/`CatPrint`/`MStrPrint`
// by name.
//
//   Print(fmt, ...)              — printf to stdout.
//   StrPrint(dst, fmt, ...)      — sprintf into `dst`; returns `dst`.
//   CatPrint(dst, fmt, ...)      — sprintf appended at `dst + StrLen(dst)`; returns `dst`.
//   MStrPrint(fmt, ...)          — asprintf into a fresh right-sized heap buffer.

U0 Print(U8 *fmt, ...);
U8 *StrPrint(U8 *dst, U8 *fmt, ...);
U8 *CatPrint(U8 *dst, U8 *fmt, ...);
U8 *MStrPrint(U8 *fmt, ...);

#endif
