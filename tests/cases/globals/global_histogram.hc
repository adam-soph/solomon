// Global histogram array built by a function.
#define BINS 4
I64 g_hist[BINS];

U0 Tally(I64 v) { if (v >= 0 && v < BINS) g_hist[v]++; }

I64 data[8] = {0,1,2,3,0,1,0,2};
I64 i;
for (i = 0; i < 8; i++) Tally(data[i]);
for (i = 0; i < BINS; i++) "%d ", g_hist[i];
"\n";
