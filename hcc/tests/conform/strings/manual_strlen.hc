// manual_strlen.hc — hand-rolled strlen without stdlib

#include <stdio.hh>
U8 *s = "hello world";
I64 n = 0;
while (s[n] != 0) n++;
"%d\n", n;

U8 *empty = "";
I64 m = 0;
while (empty[m] != 0) m++;
"%d\n", m;
