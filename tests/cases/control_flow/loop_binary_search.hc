// Binary search with while loop.
I64 BSearch(I64 arr[], I64 n, I64 target)
{
  I64 lo = 0, hi = n - 1;
  while (lo <= hi) {
    I64 mid = (lo + hi) / 2;
    if (arr[mid] == target)
      return mid;
    else if (arr[mid] < target)
      lo = mid + 1;
    else
      hi = mid - 1;
  }
  return -1;
}

I64 a[8];
a[0] = 1; a[1] = 3; a[2] = 5; a[3] = 7;
a[4] = 9; a[5] = 11; a[6] = 13; a[7] = 15;
"%d\n", BSearch(a, 8, 7);
"%d\n", BSearch(a, 8, 1);
"%d\n", BSearch(a, 8, 15);
"%d\n", BSearch(a, 8, 4);
