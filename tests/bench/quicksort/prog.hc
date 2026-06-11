// In-place quicksort of a generated array, repeated; print a position-weighted checksum.
#define N 20000
I64 data[N];
U0 Quick(I64 *a, I64 lo, I64 hi) {
  if (lo >= hi) return;
  I64 p = a[(lo + hi) / 2], i = lo, j = hi;
  while (i <= j) {
    while (a[i] < p) i++;
    while (a[j] > p) j--;
    if (i <= j) { I64 t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
  }
  Quick(a, lo, j);
  Quick(a, i, hi);
}
I64 i, rep, total = 0;
for (rep = 0; rep < 8; rep++) {
  for (i = 0; i < N; i++) data[i] = (i * 1103 + 12345) % 100003;
  Quick(data, 0, N - 1);
  for (i = 0; i < N; i++) total += (i % 1000) * data[i];
}
"%d\n", total;
