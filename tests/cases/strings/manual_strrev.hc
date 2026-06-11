// manual_strrev.hc — hand-rolled string reverse
U8 buf[16];
U8 *src = "abcde";
I64 n = 0;
while (src[n] != 0) n++;
I64 i = 0;
while (i < n) { buf[i] = src[n - 1 - i]; i++; }
buf[n] = 0;
"%s\n", buf;
