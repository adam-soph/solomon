// One top-level counter reused across several loops (promotion + phi construction where one
// SSA variable threads multiple loops); the final value escapes the last loop.
I64 i, a = 0, b = 0, c = 0;
for (i = 0; i < 10; i++) a += i;
for (i = 0; i < 20; i++) b += i;
for (i = 5; i < 15; i++) c += i;
"%d %d %d %d\n", a, b, c, i;
