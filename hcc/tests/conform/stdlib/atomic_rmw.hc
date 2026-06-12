
// Single-threaded, so the CAS loops are deterministic. Each returns the OLD value.
#include <stdatomic.hh>
#include <stdio.hh>
I64 w = 0xF0;
"%X %X\n", AtomicAnd(&w, 0x3C), w;
"%X %X\n", AtomicOr(&w, 0x03), w;
"%X %X\n", AtomicXor(&w, 0xFF), w;
I64 flag = 0;
"%d %d\n", AtomicFlagTestAndSet(&flag), AtomicFlagTestAndSet(&flag);
AtomicFlagClear(&flag);
"%d\n", flag;
