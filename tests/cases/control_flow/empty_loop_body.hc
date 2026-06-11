// Empty loop bodies (semicolon body).
I64 n = 0;
while (n++ < 5);
"n=%d\n", n;

// for with empty body — count via side effect in condition.
I64 i = 0;
for (; i < 8; i++);
"i=%d\n", i;
