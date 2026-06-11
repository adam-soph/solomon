// swap_generic.hc — generic Swap<T> in place
U0 Swap<type T>(T *a, T *b) { T tmp = *a; *a = *b; *b = tmp; }
I64 x = 3, y = 7;
Swap(&x, &y);
"%d %d\n", x, y;
F64 p = 1.5, q = 2.5;
Swap(&p, &q);
"%.1f %.1f\n", p, q;
U8 *s1 = "hello", *s2 = "world";
Swap(&s1, &s2);
"%s %s\n", s1, s2;
