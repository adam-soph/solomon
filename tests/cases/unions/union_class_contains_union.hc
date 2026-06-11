// union_class_contains_union.hc — a class whose only field is a named union
union Val { I64 i; U64 u; };
class Box { union Val v; };
Box b; b.v.i = -100;
"%d\n", b.v.i;
