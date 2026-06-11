// union_bytes_round_trip.hc — write byte-by-byte, read as U64
union Byt { U8 b[8]; U64 all; };
Byt x;
x.b[0] = 0x01; x.b[1] = 0x00; x.b[2] = 0x00; x.b[3] = 0x00;
x.b[4] = 0x00; x.b[5] = 0x00; x.b[6] = 0x00; x.b[7] = 0x00;
// little-endian: x.all == 1
"%d\n", (x.all == 1) ? 1 : 0;
