
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "a,b,c");
U8 *save;
U8 *tok = StrTokR(buf, ",", &save);
while (tok != NULL) {
    "%s\n", tok;
    tok = StrTokR(NULL, ",", &save);
}
