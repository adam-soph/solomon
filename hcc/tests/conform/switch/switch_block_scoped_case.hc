// switch case with block-scoped variables.

#include <stdio.hh>
I64 op = 1;
I64 result;
switch (op) {
  case 0: {
    I64 tmp = 10 * 2;
    result = tmp;
    break;
  }
  case 1: {
    I64 tmp = 7 * 7;
    result = tmp;
    break;
  }
  default:
    result = -1;
}
"result=%d\n", result;
