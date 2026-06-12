// array_of_classes.hc — array of class instances

#include <stdio.hh>
class Point { I64 x; I64 y; };
Point pts[3];
pts[0].x = 1; pts[0].y = 2;
pts[1].x = 3; pts[1].y = 4;
pts[2].x = 5; pts[2].y = 6;
I64 i;
for (i = 0; i < 3; i++) "%d %d\n", pts[i].x, pts[i].y;
