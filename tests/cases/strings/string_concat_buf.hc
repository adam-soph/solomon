// string_concat_buf.hc — concatenate two strings by hand into a buffer
U8 buf[64];
U8 *first = "hello";
U8 *second = " world";
I64 i = 0, j = 0;
while (first[i] != 0) { buf[j] = first[i]; i++; j++; }
i = 0;
while (second[i] != 0) { buf[j] = second[i]; i++; j++; }
buf[j] = 0;
"%s\n", buf;
