// for and while loops used together in one program.

#include <stdio.hh>
I64 arr[5];
I64 i;
for (i = 0; i < 5; i++)
  arr[i] = (i + 1) * (i + 1);

I64 idx = 0;
while (idx < 5) {
  "%d ", arr[idx];
  idx++;
}
"\n";
