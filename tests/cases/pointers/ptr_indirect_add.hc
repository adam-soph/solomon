// Add values through pointers; verify the result.
I64 a = 3, b = 7;
I64 *pa = &a, *pb = &b;
I64 sum = *pa + *pb;
"%d\n", sum;
*pa += *pb;
"%d\n", a;
