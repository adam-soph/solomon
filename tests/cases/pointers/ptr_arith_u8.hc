// Pointer arithmetic with U8* stride (1 byte per step).
U8 buf[4] = {1, 2, 3, 4};
U8 *p = buf;
"%d %d %d %d\n", *p, *(p+1), *(p+2), *(p+3);
