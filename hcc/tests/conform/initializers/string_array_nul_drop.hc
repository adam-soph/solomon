// U8 s[3]="abc" — the NUL is dropped (exactly 3 chars fit).

#include <stdio.hh>
U8 s[3] = "abc";
"%d %d %d\n", (I64)s[0], (I64)s[1], (I64)s[2];
