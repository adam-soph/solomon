// Float accumulation in a loop — native and the interpreter must agree bit-for-bit on F64.
F64 sum = 0.0, x;
I64 i;
for (i = 1; i <= 20; i++) { x = i; sum += 1.0 / (x * x); }
"%.6f\n", sum;
