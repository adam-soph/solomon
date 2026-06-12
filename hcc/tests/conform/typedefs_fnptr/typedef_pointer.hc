// typedef of a pointer type.

#include <stdio.hh>
typedef I64 *IntPtr;
I64 val = 77;
IntPtr p = &val;
"*p=%d\n", *p;
*p = 99;
"val=%d\n", val;
