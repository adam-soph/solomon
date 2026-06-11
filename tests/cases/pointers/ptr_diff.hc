// Pointer difference gives an element count, not a byte count.
I64 arr[6] = {0, 1, 2, 3, 4, 5};
I64 *p = arr;
I64 *q = arr + 4;
"%d\n", q - p;
