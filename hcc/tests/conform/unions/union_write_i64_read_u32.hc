// union_write_i64_read_u32.hc — write I64, read the low U32

#include <stdio.hh>
union IU { I64 i; U32 lo; };
IU x; x.i = 0x100000007;
// low 32 bits = 7
"%d\n", x.lo;
