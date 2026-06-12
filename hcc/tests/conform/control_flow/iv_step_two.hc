// An induction variable with step != 1 (`k += 2`) indexing an array.

#include <stdio.hh>
#define N 20
I64 arr[N];
I64 k, sum = 0;
for (k = 0; k < N; k++) arr[k] = k + 1;
for (k = 0; k < N; k += 2) sum += arr[k];
"%d\n", sum;
