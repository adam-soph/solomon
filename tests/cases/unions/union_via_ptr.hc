// union_via_ptr.hc — access union fields through a pointer
union Reg { U64 r; U32 e[2]; };
Reg z; z.r = 0xAABBCCDD11223344;
Reg *p = &z;
"%x %x\n", p->e[0], p->e[1];
