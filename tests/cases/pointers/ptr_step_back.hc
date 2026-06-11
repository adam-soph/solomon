// Step a pointer backward through an array.
I64 arr[4] = {10, 20, 30, 40};
I64 *p = arr + 3;
I64 i;
for (i = 0; i < 4; i++) {
  "%d ", *p;
  p--;
}
"\n";
