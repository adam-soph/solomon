// union_all_widths.hc — union with fields at every width, write I64 read narrower
union All { I64 i64; U32 u32; U16 u16; U8 u8; };
All x; x.i64 = 0x0102030405060708;
"%d\n", x.u8;   // low byte = 8
"%d\n", x.u16;  // low 16 bits = 0x0708 = 1800
"%d\n", x.u32;  // low 32 bits = 0x05060708 = 84281096
