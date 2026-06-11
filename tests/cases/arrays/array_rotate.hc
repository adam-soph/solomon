// Rotate an array left by k positions.
I64 a[6] = {1, 2, 3, 4, 5, 6};
I64 k = 2, n = 6;
I64 tmp[6];
I64 i;
for (i = 0; i < n; i++) tmp[i] = a[(i + k) % n];
for (i = 0; i < n; i++) "%d ", tmp[i];
"\n";
