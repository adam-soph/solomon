// Pointer equality and inequality comparisons.
I64 arr[3] = {7, 8, 9};
I64 *p = arr;
I64 *q = arr;
I64 *r = arr + 1;
"%d %d %d\n", p == q, p != r, r == arr + 1;
