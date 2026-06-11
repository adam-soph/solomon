// Point at different variables without modifying them via reassigning the pointer.
I64 x = 11, y = 22;
I64 *p = &x;
"%d\n", *p;
p = &y;
"%d\n", *p;
