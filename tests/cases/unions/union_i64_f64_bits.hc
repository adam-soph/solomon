// union_i64_f64_bits.hc — write F64 1.0, read back as I64 (IEEE bits)
union Pun { F64 f; I64 i; };
Pun p; p.f = 1.0;
// 1.0 in IEEE 754 = 0x3FF0000000000000
"%d\n", (p.i == 0x3FF0000000000000) ? 1 : 0;
