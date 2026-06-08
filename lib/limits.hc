#ifndef _LIMITS_HC
#define _LIMITS_HC
// limits.hc — C `<limits.h>` / `<stdint.h>`: the ranges of the integer types. HolyC's
// integers are explicit-width (`I8`..`I64`, `U8`..`U64`), so the limits are named by width
// like `<stdint.h>` (`I8_MAX`, `U64_MAX`, …) rather than C's `INT_MAX`/`LONG_MAX`, which
// would be ambiguous here (HolyC's default `int` is 64-bit). Include with
// `#include <limits.hc>`.

#define CHAR_BIT 8 // bits per byte

#define I8_MIN (-128)
#define I8_MAX 127
#define U8_MAX 255

#define I16_MIN (-32768)
#define I16_MAX 32767
#define U16_MAX 65535

#define I32_MIN (-2147483648)
#define I32_MAX 2147483647
#define U32_MAX 4294967295

#define I64_MAX 9223372036854775807
#define I64_MIN (-I64_MAX - 1)       // -9223372036854775808 (the literal alone would overflow)
#define U64_MAX 18446744073709551615 // 2^64-1, stored as the all-ones bit pattern

#endif
