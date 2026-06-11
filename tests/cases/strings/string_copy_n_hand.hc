// string_copy_n_hand.hc — copy at most N chars manually
U8 buf[8];
U8 *src = "abcdefgh";
I64 n = 4, i = 0;
while (i < n && src[i] != 0) { buf[i] = src[i]; i++; }
buf[i] = 0;
"%s\n", buf;
