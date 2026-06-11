// Reuse the same pointer variable to point at different heap blocks.
I64 *p = MAlloc(sizeof(I64));
*p = 100;
"%d\n", *p;
Free(p);
p = MAlloc(sizeof(I64));
*p = 200;
"%d\n", *p;
Free(p);
