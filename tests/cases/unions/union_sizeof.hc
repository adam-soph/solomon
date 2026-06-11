// union_sizeof.hc — sizeof a union equals its largest member
union Big { U8 a; U16 b; U32 c; U64 d; };
// all 8-byte max member -> 8
"%d\n", sizeof(Big);
