// count_vowels.hc — count vowels in a string

#include <math.hh>
#include <stdio.hh>
U8 *s = "Hello World";
I64 count = 0, i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if (c == 'a' || c == 'e' || c == 'i' || c == 'o' || c == 'u' ||
        c == 'A' || c == 'E' || c == 'I' || c == 'O' || c == 'U')
        count++;
    i++;
}
"%d\n", count;
