// Array passed as a by-reference parameter; function sums it.
I64 SumArr(I64 a[], I64 n) {
  I64 s = 0, i;
  for (i = 0; i < n; i++) s += a[i];
  return s;
}

I64 data[5] = {1, 2, 3, 4, 5};
"%d\n", SumArr(data, 5);
