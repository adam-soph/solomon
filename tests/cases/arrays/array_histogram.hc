// Build a histogram (frequency count) from a small data array.
I64 data[10] = {1, 3, 2, 1, 3, 3, 2, 1, 1, 2};
I64 hist[5] = {0};
I64 i;
for (i = 0; i < 10; i++) hist[data[i]]++;
for (i = 1; i <= 3; i++) "%d ", hist[i];
"\n";
