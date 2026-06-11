// union_designated_init.hc — designated initializer for a union (first field)
union IU { I64 i; U64 u; };
IU x = {.i = -42};
"%d\n", x.i;
