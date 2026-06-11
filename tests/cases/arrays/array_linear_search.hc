// Linear search: return index of a target value (-1 if missing).
I64 Find(I64 a[], I64 n, I64 target) {
  I64 i;
  for (i = 0; i < n; i++)
    if (a[i] == target) return i;
  return -1;
}

I64 data[6] = {3, 1, 4, 1, 5, 9};
"%d\n", Find(data, 6, 5);
"%d\n", Find(data, 6, 7);
