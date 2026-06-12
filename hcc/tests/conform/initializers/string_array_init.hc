// String into array: U8 s[]="abc" — length 4, last is NUL.

#include <stdio.hh>
U8 s[] = "abc";
"%d %d %d %d\n", (I64)s[0], (I64)s[1], (I64)s[2], (I64)s[3];
