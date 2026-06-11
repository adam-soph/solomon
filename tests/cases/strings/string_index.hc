// string_index.hc — indexing into a string literal
U8 *s = "abcde";
I64 i = 0;
while (i < 5) {
    "%c", s[i];
    i++;
}
"\n";
"%d\n", s[0];
"%d\n", s[4];
