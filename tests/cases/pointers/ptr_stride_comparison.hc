// Show that two pointers advanced by the same count compare equal.
I64 arr[5] = {1, 2, 3, 4, 5};
I64 *p = arr + 1;
I64 *q = arr;
q++;
"%d\n", p == q;
