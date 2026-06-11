// Reverse an array in place.
I64 a[5] = {1, 2, 3, 4, 5};
I64 lo = 0, hi = 4;
while (lo < hi) {
  I64 tmp = a[lo]; a[lo] = a[hi]; a[hi] = tmp;
  lo++; hi--;
}
I64 i;
for (i = 0; i < 5; i++) "%d ", a[i];
"\n";
