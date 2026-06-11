// Sum of gcd(i, j) over a grid — Euclid's algorithm in a tight loop.
I64 Gcd(I64 a, I64 b) { while (b) { I64 t = b; b = a % b; a = t; } return a; }
I64 i, j, total = 0;
for (i = 1; i <= 900; i++) for (j = 1; j <= 900; j++) total += Gcd(i, j);
"%d\n", total;
