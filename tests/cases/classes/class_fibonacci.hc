// class_fibonacci.hc — class-based fibonacci accumulator
class Fib { I64 prev; I64 cur; };
Fib f; f.prev = 0; f.cur = 1;
I64 i;
for (i = 0; i < 8; i++) { I64 nx = f.prev + f.cur; f.prev = f.cur; f.cur = nx; }
"%d\n", f.prev;
