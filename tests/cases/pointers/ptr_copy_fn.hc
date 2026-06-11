// Copy array through pointers in a function.
U0 CopyArr(I64 *dst, I64 *src, I64 n) {
  I64 i;
  for (i = 0; i < n; i++) dst[i] = src[i];
}

I64 src[4] = {11, 22, 33, 44};
I64 dst[4];
CopyArr(dst, src, 4);
"%d %d %d %d\n", dst[0], dst[1], dst[2], dst[3];
