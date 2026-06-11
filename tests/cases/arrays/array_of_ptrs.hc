// Array of I64 pointers.
I64 a = 10, b = 20, c = 30;
I64 *ptrs[3] = {&a, &b, &c};
I64 sum = 0, i;
for (i = 0; i < 3; i++) sum += *ptrs[i];
"%d\n", sum;
