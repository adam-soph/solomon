// Global class that contains another class as a field.
// Note: 'start' and 'end' are HolyC keywords; use 'a' and 'b' as field names.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
class Line { Pt a; Pt b; };

Line g_line;

g_line.a.x = 0; g_line.a.y = 0;
g_line.b.x = 3; g_line.b.y = 4;

I64 dx = g_line.b.x - g_line.a.x;
I64 dy = g_line.b.y - g_line.a.y;
"%d %d\n", dx, dy;
