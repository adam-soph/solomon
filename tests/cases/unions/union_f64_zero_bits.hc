// union_f64_zero_bits.hc — F64 0.0 has all zero bits
union FloatBits { F64 f; U64 bits; };
FloatBits fb; fb.f = 0.0;
"%d\n", (fb.bits == 0) ? 1 : 0;
