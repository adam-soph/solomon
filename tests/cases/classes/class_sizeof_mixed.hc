// class_sizeof_mixed.hc — sizeof a class with mixed I64 and F64
class Mixed { I64 a; F64 b; I64 c; };
// 3 x 8 = 24 bytes (all naturally 8-byte aligned)
"%d\n", sizeof(Mixed);
