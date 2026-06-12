
#include <stdio.hh>
#include <string.hh>
U8 *s = "hello";
// An empty needle matches at the start
U8 *p = StrFind(s, "");
"%d\n", p == s;
