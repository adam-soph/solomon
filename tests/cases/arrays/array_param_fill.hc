// Function fills an array passed by reference.
U0 Fill(I64 a[], I64 n) {
  I64 i;
  for (i = 0; i < n; i++) a[i] = i * i;
}

I64 buf[5];
Fill(buf, 5);
I64 i;
for (i = 0; i < 5; i++) "%d ", buf[i];
"\n";
