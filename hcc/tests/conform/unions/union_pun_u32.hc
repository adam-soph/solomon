// union_pun_u32.hc — type-pun U64 as two U32s

#include <stdio.hh>
union Reg { U64 r; U32 e[2]; };
Reg z; z.r = 0x1122334455667788;
"%x %x\n", z.e[0], z.e[1];
