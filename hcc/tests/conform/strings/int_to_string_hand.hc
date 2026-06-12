// int_to_string_hand.hc — convert integer to decimal string by hand

#include <stdio.hh>
I64 n = 9876;
U8 tmp[24];
I64 i = 0;
if (n == 0) { tmp[i++] = '0'; }
else {
    I64 v = n;
    while (v > 0) { tmp[i++] = '0' + v % 10; v /= 10; }
    // reverse
    I64 a = 0, b = i - 1;
    while (a < b) { U8 t = tmp[a]; tmp[a] = tmp[b]; tmp[b] = t; a++; b--; }
}
tmp[i] = 0;
"%s\n", tmp;
