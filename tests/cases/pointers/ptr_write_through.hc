// Write through a pointer and verify both the original and pointer see the change.
I64 x = 10;
I64 *p = &x;
*p = 42;
"x=%d *p=%d\n", x, *p;
