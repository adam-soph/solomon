// Copy one array to another element by element.
I64 src[4] = {7, 14, 21, 28};
I64 dst[4];
I64 i;
for (i = 0; i < 4; i++) dst[i] = src[i];
for (i = 0; i < 4; i++) "%d ", dst[i];
"\n";
