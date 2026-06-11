// count_chars.hc — count occurrences of a specific character
U8 *s = "mississippi";
I64 count = 0, i = 0;
while (s[i] != 0) {
    if (s[i] == 's') count++;
    i++;
}
"%d\n", count;
