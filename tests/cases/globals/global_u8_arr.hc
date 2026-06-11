// Global U8 array; fill via a function, print.
#define N 5
U8 g_buf[N];

U0 Fill() {
  I64 i;
  for (i = 0; i < N; i++) g_buf[i] = (U8)(i * 3 + 1);
}

Fill();
I64 i;
for (i = 0; i < N; i++) "%d ", (I64)g_buf[i];
"\n";
