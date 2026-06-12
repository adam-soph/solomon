
#include <stdio.hh>
#include <string.hh>
U8 *b = MAlloc(32); StrCpy(b, "hi"); StrCat(b, "!");
"%s %d\n", b, StrLen(b);
