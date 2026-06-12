// Function takes a stride argument to step through an array.

#include <stdio.hh>
I64 StridedSum(I64 *arr, I64 n, I64 stride) {
  I64 s = 0, i;
  for (i = 0; i < n; i++) s += arr[i * stride];
  return s;
}

I64 data[9] = {1,2,3,4,5,6,7,8,9};
"%d\n", StridedSum(data, 9, 1);
"%d\n", StridedSum(data, 3, 3);
