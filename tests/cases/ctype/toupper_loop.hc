#include <ctype.hc>
// convert all lowercase letters to uppercase
U8 *s = "Hello World!";
I64 i = 0;
while (s[i] != 0) {
    "%c", ToUpper(s[i]);
    i++;
}
"\n";
