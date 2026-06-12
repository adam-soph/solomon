// Pointer arithmetic with stride of a class (sizeof(Point) = 16).

#include <stdio.hh>
class Point {
  I64 x;
  I64 y;
};

Point arr[3];
arr[0].x = 1; arr[0].y = 2;
arr[1].x = 3; arr[1].y = 4;
arr[2].x = 5; arr[2].y = 6;

Point *p = arr;
"%d %d\n", p->x, p->y;
p++;
"%d %d\n", p->x, p->y;
p++;
"%d %d\n", p->x, p->y;
