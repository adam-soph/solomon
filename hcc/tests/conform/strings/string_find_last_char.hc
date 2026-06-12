// string_find_last_char.hc — find last occurrence of a char

#include <stdio.hh>
U8 *s = "abcabc";
I64 idx = -1, i = 0;
while (s[i] != 0) {
    if (s[i] == 'c') idx = i;
    i++;
}
"%d\n", idx;
