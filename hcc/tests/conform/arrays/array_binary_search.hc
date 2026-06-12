// Binary search in a sorted array.

#include <stdio.hh>
I64 BSearch(I64 a[], I64 n, I64 target) {
  I64 lo = 0, hi = n - 1;
  while (lo <= hi) {
    I64 mid = (lo + hi) / 2;
    if (a[mid] == target) return mid;
    if (a[mid] < target) lo = mid + 1;
    else hi = mid - 1;
  }
  return -1;
}

I64 sorted[7] = {1, 3, 5, 7, 9, 11, 13};
"%d\n", BSearch(sorted, 7, 7);
"%d\n", BSearch(sorted, 7, 6);
