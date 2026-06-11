// repeat_char.hc — build a string of repeated characters
U8 buf[12];
I64 i = 0;
while (i < 10) { buf[i] = '*'; i++; }
buf[10] = 0;
"%s\n", buf;
