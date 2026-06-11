// A function fills an array through a pointer parameter.
U0 Fill(I64 *dst, I64 n, I64 val) {
  I64 i;
  for (i = 0; i < n; i++)
    dst[i] = val;
}

I64 buf[5];
Fill(buf, 5, 7);
I64 i;
for (i = 0; i < 5; i++)
  "%d ", buf[i];
"\n";
