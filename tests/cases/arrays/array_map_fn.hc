// Apply a function to every element (map pattern).
I64 Square(I64 x) { return x * x; }

I64 a[5] = {1, 2, 3, 4, 5};
I64 b[5];
I64 i;
for (i = 0; i < 5; i++) b[i] = Square(a[i]);
for (i = 0; i < 5; i++) "%d ", b[i];
"\n";
