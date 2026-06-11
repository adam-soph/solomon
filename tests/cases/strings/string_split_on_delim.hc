// string_split_on_delim.hc — split a string on a delimiter by hand
U8 buf[64];
U8 *s = "a,b,c,d";
// copy into mutable buffer
I64 i = 0;
while (s[i] != 0) { buf[i] = s[i]; i++; }
buf[i] = 0;
// scan and replace comma with NUL, print each token
I64 n = i;
I64 tok = 0;
i = 0;
while (i <= n) {
    if (buf[i] == ',' || buf[i] == 0) {
        buf[i] = 0;
        "%s\n", buf + tok;
        tok = i + 1;
    }
    i++;
}
