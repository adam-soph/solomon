// union_named_member.hc — named union member inside a class
union Num { I64 i; U64 u; };
class Tagged { I64 tag; union Num m; };
Tagged t; t.tag = 1; t.m.i = -5;
"%d %d\n", t.tag, t.m.i;
