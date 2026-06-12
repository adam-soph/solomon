// Function pointer used as a comparator passed to a sort helper.

#include <stdio.hh>
#include <stdlib.hh>
I64 Cmp_Asc(I64 a, I64 b) { return (a < b) ? -1 : (a > b) ? 1 : 0; }
I64 Cmp_Desc(I64 a, I64 b) { return (a > b) ? -1 : (a < b) ? 1 : 0; }

// Simple insertion sort using a comparator fn-ptr.
U0 InsSort(I64 arr[], I64 n, I64 (*cmp)(I64, I64))
{
  I64 i, j;
  for (i = 1; i < n; i++) {
    I64 key = arr[i];
    j = i - 1;
    while (j >= 0 && cmp(arr[j], key) > 0) {
      arr[j+1] = arr[j];
      j--;
    }
    arr[j+1] = key;
  }
}

I64 a[5];
a[0]=3; a[1]=1; a[2]=4; a[3]=1; a[4]=5;
InsSort(a, 5, &Cmp_Asc);
I64 i;
for (i = 0; i < 5; i++) "%d ", a[i];
"\n";

I64 b[5];
b[0]=3; b[1]=1; b[2]=4; b[3]=1; b[4]=5;
InsSort(b, 5, &Cmp_Desc);
for (i = 0; i < 5; i++) "%d ", b[i];
"\n";
