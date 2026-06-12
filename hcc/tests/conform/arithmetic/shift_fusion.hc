// Shifted-register operand fusion: `a OP (b << k)` / `a OP (b >> k)` folds the constant shift
// into the consumer (arm64 `add x,a,b,lsl #k` etc.). Covers every fusable op and several widths,
// so the post-fusion narrowing `Cast` is exercised too. Operands arrive via calls so the shift
// and its consumer stay adjacent in the IR (what the fusion requires).

#include <stdio.hh>
U64 xor_shl(U64 a, U64 b) { return a ^ (b << 13); }   // xorshift shape
I64 add_shl(I64 a, I64 b) { return a + (b << 5); }    // djb2 shape: (h<<5)+h via a==b
I64 sub_shl(I64 a, I64 b) { return a - (b << 3); }    // non-commutative: only `a - (b<<k)`
I64 and_shr(I64 a, I64 b) { return a & (b >> 4); }    // arithmetic (signed) shift
U64 or_lsr(U64 a, U64 b)  { return a | (b >> 7); }    // logical (unsigned) shift
I32 add_shl32(I32 a, I32 b) { return a + (b << 5); }  // narrow: result truncated to I32
U8  and_shr8(U8 a, U8 b)  { return a & (b >> 2); }    // narrow: result truncated to U8

"%d\n", xor_shl(0x9E3779B97F4A7C15, 0x9E3779B97F4A7C15);
"%d\n", add_shl(7, 7);
"%d\n", sub_shl(100, 3);
"%d\n", and_shr(0xFF0, -16);
"%d\n", or_lsr(0xF000, 0x100);
"%d\n", add_shl32(1000000000, 100000000);
"%d\n", and_shr8(0xFF, 0xF0);
