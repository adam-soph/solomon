// union_copy.hc — assign one union to another copies all bytes
union W { I64 a; U64 u; };
W x; x.a = 77;
W y = x;
y.a = 0;
"%d %d\n", x.a, y.a;
