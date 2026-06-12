
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "foo");
U8 *r = StrCat(buf, "bar");
"%s\n", r;
