// Pointer arithmetic with I32* stride (4 bytes per step).
I32 arr[4] = {100, 200, 300, 400};
I32 *p = arr;
"%d %d %d %d\n", *p, *(p+1), *(p+2), *(p+3);
