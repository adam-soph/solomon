// caesar_cipher.hc — ROT13 / Caesar cipher by hand

#include <stdio.hh>
U8 buf[32];
U8 *s = "Hello";
I64 shift = 3;
I64 i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if (c >= 'a' && c <= 'z') buf[i] = (c - 'a' + shift) % 26 + 'a';
    else if (c >= 'A' && c <= 'Z') buf[i] = (c - 'A' + shift) % 26 + 'A';
    else buf[i] = c;
    i++;
}
buf[i] = 0;
"%s\n", buf;
