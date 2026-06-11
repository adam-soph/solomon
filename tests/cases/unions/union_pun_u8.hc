// union_pun_u8.hc — type-pun U64 as eight bytes
union Bytes { U64 all; U8 b[8]; };
Bytes x; x.all = 0x0102030405060708;
"%d %d %d %d\n", x.b[0], x.b[1], x.b[2], x.b[3];
