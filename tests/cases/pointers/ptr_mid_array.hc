// Pointer into the middle of an array; index forward and backward from there.
I64 arr[5] = {1, 2, 3, 4, 5};
I64 *mid = arr + 2;
"%d %d %d\n", *(mid - 1), *mid, *(mid + 1);
