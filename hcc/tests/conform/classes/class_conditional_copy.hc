// class_conditional_copy.hc — copy class conditionally based on a field value

#include <stdio.hh>
class Val { I64 n; };
Val a; a.n = 10; Val b; b.n = 20;
Val picked;
if (a.n > b.n) picked = a; else picked = b;
"%d\n", picked.n;
