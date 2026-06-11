// Function takes a pointer + length and returns the sum.
I64 Sum(I64 *arr, I64 n) {
  I64 s = 0, i;
  for (i = 0; i < n; i++) s += arr[i];
  return s;
}

I64 data[5] = {1, 2, 3, 4, 5};
"%d\n", Sum(data, 5);
"%d\n", Sum(data + 1, 3);
