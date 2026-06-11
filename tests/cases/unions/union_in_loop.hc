// union_in_loop.hc — write union in a loop, accumulate via the integer view
union Acc { I64 sum; U64 raw; };
Acc a; a.sum = 0;
I64 i;
for (i = 1; i <= 5; i++) a.sum = a.sum + i;
"%d\n", a.sum;
