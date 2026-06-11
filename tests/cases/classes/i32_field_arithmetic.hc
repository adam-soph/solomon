// i32_field_arithmetic.hc — I32 field arithmetic and truncation
class Narrow { I32 x; I32 y; };
Narrow n; n.x = 100000; n.y = -100000;
"%d %d\n", n.x, n.y;
I32 sum = n.x + n.y;
"%d\n", sum;
