// U8 array initialized with byte values; cast to I64 when printing.
U8 a[5] = {10, 20, 30, 40, 50};
I64 i;
for (i = 0; i < 5; i++) "%d ", (I64)a[i];
"\n";
