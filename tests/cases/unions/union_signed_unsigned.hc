// union_signed_unsigned.hc — write I64 -1 read as U64 max
union SU { I64 s; U64 u; };
SU x; x.s = -1;
// u = 0xFFFFFFFFFFFFFFFF = 18446744073709551615 (print as signed for determinism)
I64 r = (x.u == 0xFFFFFFFFFFFFFFFF) ? 1 : 0;
"%d\n", r;
