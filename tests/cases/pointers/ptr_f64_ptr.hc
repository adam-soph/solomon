// F64 pointer: write and read back a float through a pointer.
F64 x = 3.14;
F64 *p = &x;
*p = *p * 2.0;
"%f\n", x;
