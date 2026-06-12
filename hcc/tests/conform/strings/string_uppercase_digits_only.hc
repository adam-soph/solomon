// string_uppercase_digits_only.hc — only uppercase letters, skip everything else

#include <stdio.hh>
U8 *s = "Hello World 123";
I64 i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if (c >= 'A' && c <= 'Z') "%c", c;
    i++;
}
"\n";
