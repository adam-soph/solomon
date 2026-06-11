// Bubble sort; print before and after.
I64 a[6] = {5, 3, 8, 1, 9, 2};
I64 i, j;
for (i = 0; i < 6; i++) "%d ", a[i];
"\n";
for (i = 0; i < 5; i++)
  for (j = 0; j < 5 - i; j++)
    if (a[j] > a[j+1]) { I64 t = a[j]; a[j] = a[j+1]; a[j+1] = t; }
for (i = 0; i < 6; i++) "%d ", a[i];
"\n";
