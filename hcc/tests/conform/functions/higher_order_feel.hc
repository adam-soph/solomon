// Apply a function to an array by passing array pointer.

#include <stdio.hh>
#include <stdlib.hh>
U0 ApplyDouble(I64 arr[], I64 n)
{
  I64 i;
  for (i = 0; i < n; i++)
    arr[i] *= 2;
}

I64 a[4];
a[0]=1; a[1]=2; a[2]=3; a[3]=4;
ApplyDouble(a, 4);
I64 i;
for (i = 0; i < 4; i++)
  "%d ", a[i];
"\n";
