// string_find_char.hc — find first occurrence of a char by hand
U8 *s = "abcabc";
I64 idx = -1, i = 0;
while (s[i] != 0) {
    if (s[i] == 'c') { idx = i; break; }
    i++;
}
"%d\n", idx;
