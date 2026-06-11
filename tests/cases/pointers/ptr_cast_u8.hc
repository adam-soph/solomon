// Cast I64* to U8* and read individual bytes of a multi-byte value.
I64 v = 0x0102030405060708;
U8 *p = (U8 *)&v;
// Little-endian: byte 0 is the least-significant byte.
I64 b0 = p[0];
I64 b7 = p[7];
"%d %d\n", b0, b7;
