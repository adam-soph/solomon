// union_nested_anon_deep.hc — class with a named + anonymous union
class Token {
  I64 kind;
  union { I64 ival; U64 uval; };
};
Token t; t.kind = 3; t.uval = 0xCAFE;
"%d %d\n", t.kind, t.uval;
