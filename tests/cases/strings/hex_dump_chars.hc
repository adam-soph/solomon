// hex_dump_chars.hc — print hex values of characters
U8 *s = "Hi!";
I64 i = 0;
while (s[i] != 0) {
    "%x ", s[i];
    i++;
}
"\n";
