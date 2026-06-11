// union_u32_hi_lo.hc — union with two U32 halves; write each independently
union Half { U64 all; U32 parts[2]; };
Half h; h.parts[0] = 0xAABBCCDD; h.parts[1] = 0x11223344;
"%x %x\n", h.parts[0], h.parts[1];
