// union_positional_init.hc — positional brace init into a union (first member)
union Reg { U64 r; U32 e[2]; };
Reg x = {0xDEADBEEF00000000};
"%x\n", x.e[1];  // high 32 bits = 0xDEADBEEF
