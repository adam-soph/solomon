// Heap allocation, write through pointer, free.
I64 *p = MAlloc(sizeof(I64) * 3);
p[0] = 10;
p[1] = 20;
p[2] = 30;
"%d %d %d\n", p[0], p[1], p[2];
Free(p);
