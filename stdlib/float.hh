#ifndef _FLOAT_HH
#define _FLOAT_HH
// float.hh — C `<float.h>`: the characteristics of the floating-point type. HolyC has a
// single binary64 float, `F64` (C's `double`), so these describe it. The constants are
// named `F64_*`, with `DBL_*` aliases for the magnitudes to match C. Include with
// `#include <float.hc>`.

#define FLT_RADIX 2 // base of the exponent

#define F64_MANT_DIG   53 // bits of mantissa, including the implicit leading 1
#define F64_DIG        15 // decimal digits that always round-trip
#define F64_MIN_EXP    (-1021)
#define F64_MAX_EXP    1024
#define F64_MIN_10_EXP (-307)
#define F64_MAX_10_EXP 308

#define F64_MAX      1.7976931348623157e308  // largest finite F64
#define F64_MIN      2.2250738585072014e-308 // smallest positive *normal* F64
#define F64_EPSILON  2.220446049250313e-16   // 2^-52: the gap from 1.0 to the next F64
#define F64_TRUE_MIN 4.9406564584124654e-324 // smallest positive *subnormal* F64

// C `<float.h>` spellings for the magnitudes.
#define DBL_MAX      F64_MAX
#define DBL_MIN      F64_MIN
#define DBL_EPSILON  F64_EPSILON
#define DBL_TRUE_MIN F64_TRUE_MIN

#endif
