// uppercase_by_hand.hc — convert lowercase to uppercase manually

#include <stdio.hh>
U8 buf[32];
U8 *s = "hello world";
I64 i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if (c >= 'a' && c <= 'z') buf[i] = c - 'a' + 'A';
    else buf[i] = c;
    i++;
}
buf[i] = 0;
"%s\n", buf;
