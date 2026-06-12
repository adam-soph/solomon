// Function taking an array parameter (decays to pointer).

#include <stdio.hh>
I64 ArraySum(I64 arr[], I64 n)
{
  I64 sum = 0, i;
  for (i = 0; i < n; i++)
    sum += arr[i];
  return sum;
}
I64 a[5];
a[0] = 1; a[1] = 2; a[2] = 3; a[3] = 4; a[4] = 5;
"%d\n", ArraySum(a, 5);
