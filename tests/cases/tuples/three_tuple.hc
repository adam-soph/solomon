// three_tuple.hc — 3-tuple with mixed I64/I64/F64
(I64, I64, F64) Stats(I64 a, I64 b) { return a+b, a*b, (a+b)/2.0; }
sum, prod, avg := Stats(7, 3);
"sum=%d prod=%d avg=%.1f\n", sum, prod, avg;
sum2, prod2, avg2 := Stats(10, 5);
"sum=%d prod=%d avg=%.1f\n", sum2, prod2, avg2;
