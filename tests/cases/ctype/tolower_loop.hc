#include <ctype.hc>
U8 *s = "Hello World!";
I64 i = 0;
while (s[i] != 0) {
    "%c", ToLower(s[i]);
    i++;
}
"\n";
