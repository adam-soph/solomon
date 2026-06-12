// lowercase_by_hand.hc — convert uppercase to lowercase manually

#include <stdio.hh>
U8 buf[32];
U8 *s = "HELLO WORLD";
I64 i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if (c >= 'A' && c <= 'Z') buf[i] = c + 32;
    else buf[i] = c;
    i++;
}
buf[i] = 0;
"%s\n", buf;
