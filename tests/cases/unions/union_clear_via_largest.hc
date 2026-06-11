// union_clear_via_largest.hc — set largest field to zero clears all bytes
union Multi { U64 all; U32 lo; U8 b[8]; };
Multi m; m.all = 0xDEADBEEFCAFEBABE;
m.all = 0;
"%d %d %d\n", m.lo, m.b[0], m.b[7];
