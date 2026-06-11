// union_two_anon.hc — two anonymous unions in one class
class Packet {
  I64 tag;
  union { I64 ival; U64 uval; };
  union { F64 fval; I64 fbits; };
};
Packet pk;
pk.tag = 2;
pk.ival = -99;
pk.fval = 1.5;
"%d %d\n", pk.tag, pk.ival;
"%d\n", (pk.fbits == 0x3FF8000000000000) ? 1 : 0;
