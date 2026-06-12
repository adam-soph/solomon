
#include <float.hh>
#include <limits.hh>
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  "%d %d %u | %d %d %u\n", I8_MIN, I8_MAX, U8_MAX, I16_MIN, I16_MAX, U16_MAX;
  "%d %d %u\n", I32_MIN, I32_MAX, U32_MAX;
  "%d %d %u\n", I64_MIN, I64_MAX, U64_MAX;
  // float characteristics must hit the canonical IEEE-754 bit patterns
  "%x %x %x %x\n", Float64bits(F64_MAX), Float64bits(F64_MIN),
                   Float64bits(F64_EPSILON), Float64bits(F64_TRUE_MIN);
  "%x %x\n", Float64bits(DBL_MAX), Float64bits(DBL_EPSILON); // C aliases
}
Main;
