// union_anon_in_class.hc — anonymous union inside a class promotes members
class Variant {
  I64 tag;
  union { I64 ival; F64 fval; };
};
Variant v; v.tag = 0; v.ival = 42;
"%d %d\n", v.tag, v.ival;
