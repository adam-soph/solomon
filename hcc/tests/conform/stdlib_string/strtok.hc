
#include <stdio.hh>
#include <string.hh>
U8 buf[32];
StrCpy(buf, "one two three");
U8 *tok = StrTok(buf, " ");
while (tok != NULL) {
    "%s\n", tok;
    tok = StrTok(NULL, " ");
}
