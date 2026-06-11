// union_nested_struct.hc — union of a small class and a raw I64
class Pair { I64 a; I64 b; };
union PackOrRaw { Pair p; I64 raw[2]; };
PackOrRaw x; x.p.a = 11; x.p.b = 22;
"%d %d\n", x.raw[0], x.raw[1];
