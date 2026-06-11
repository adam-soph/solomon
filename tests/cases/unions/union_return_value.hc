// union_return_value.hc — return a union from a function
union Duo { I64 a; U64 u; };
Duo MakeDuo(I64 v) { Duo d; d.a = v; return d; }
Duo x = MakeDuo(-10);
"%d\n", x.a;
