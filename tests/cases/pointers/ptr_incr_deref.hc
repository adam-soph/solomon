// Distinguish *p++ (reads then advances) vs (*p)++ (increments the value).
I64 arr[3] = {10, 20, 30};
I64 *p = arr;
I64 v = *p;
p++;
"%d %d\n", v, *p;

I64 y = 5;
I64 *q = &y;
(*q)++;
"%d\n", y;
