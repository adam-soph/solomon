
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "a::b:c");
U8 *ptr = buf;
U8 *tok;
while ((tok = StrSep(&ptr, ":")) != NULL) {
    "%s\n", tok;
}
