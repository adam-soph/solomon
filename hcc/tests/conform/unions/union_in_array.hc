// union_in_array.hc — array of unions; write different variants, read back

#include <stdio.hh>
union Slot { I64 i; U64 u; };
Slot arr[3];
arr[0].i = -1; arr[1].i = 0; arr[2].i = 1;
"%d %d %d\n", arr[0].i, arr[1].i, arr[2].i;
