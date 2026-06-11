// Address-of, dereference, and write-through a pointer.
I64 x = 10;
I64 *p = &x;
*p = 42;
"x=%d *p=%d\n", x, *p;

I64 arr[4] = {1, 2, 3, 4};
I64 *q = arr;
"%d %d %d\n", *q, *(q + 1), q[2];
