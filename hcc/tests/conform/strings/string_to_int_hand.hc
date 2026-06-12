// string_to_int_hand.hc — parse a decimal integer from a string by hand

#include <stdio.hh>
U8 *s = "12345";
I64 n = 0, i = 0;
while (s[i] >= '0' && s[i] <= '9') {
    n = n * 10 + (s[i] - '0');
    i++;
}
"%d\n", n;
