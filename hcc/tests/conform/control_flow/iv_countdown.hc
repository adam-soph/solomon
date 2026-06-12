// A countdown loop (`k--`) indexing an array — a negative-step induction variable.

#include <stdio.hh>
#define N 16
I64 arr[N];
I64 k, sum = 0;
for (k = 0; k < N; k++) arr[k] = k * k;
for (k = N - 1; k >= 0; k--) sum += arr[k];
"%d\n", sum;
