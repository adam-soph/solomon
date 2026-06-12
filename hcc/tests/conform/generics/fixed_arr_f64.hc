// fixed_arr_f64.hc — FixedArr<F64,4>

#include <stdio.hh>
#include <stdlib.hh>
class FixedArr<type T, int N> { T data[N]; I64 len; };
U0 FAInit<type T, int N>(FixedArr<T,N> *a) { a->len = 0; }
U0 FAPush<type T, int N>(FixedArr<T,N> *a, T x) { a->data[a->len++] = x; }
T FAAt<type T, int N>(FixedArr<T,N> *a, I64 i) { return a->data[i]; }

FixedArr<F64, 4> fs;
FAInit<F64, 4>(&fs);
FAPush<F64, 4>(&fs, 1.1);
FAPush<F64, 4>(&fs, 2.2);
FAPush<F64, 4>(&fs, 3.3);
F64 sum = 0.0;
I64 i;
for (i = 0; i < fs.len; i++) sum += FAAt<F64,4>(&fs, i);
"sum=%.1f len=%d\n", sum, fs.len;
