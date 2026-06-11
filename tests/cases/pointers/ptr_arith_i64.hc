// Pointer arithmetic with I64* stride (8 bytes per step).
I64 arr[5] = {10, 20, 30, 40, 50};
I64 *p = arr;
"%d %d %d\n", *p, *(p + 1), *(p + 4);
p = p + 2;
"%d\n", *p;
