// ptr_arithmetic_array.hc — pointer arithmetic over array of classes

#include <stdio.hh>
class Cell { I64 v; };
Cell cells[5];
I64 i;
for (i = 0; i < 5; i++) cells[i].v = i * i;
Cell *p = cells;
I64 sum = 0;
for (i = 0; i < 5; i++) { sum = sum + (p + i)->v; }
"%d\n", sum;
