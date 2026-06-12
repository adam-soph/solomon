// fixed_arr_i64_8.hc — FixedArr<I64,8>: push, at, sizeof

#include <stdio.hh>
#include <stdlib.hh>
class FixedArr<type T, int N> { T data[N]; I64 len; };
U0 FAInit<type T, int N>(FixedArr<T,N> *a) { a->len = 0; }
U0 FAPush<type T, int N>(FixedArr<T,N> *a, T x) { a->data[a->len++] = x; }
T FAAt<type T, int N>(FixedArr<T,N> *a, I64 i) { return a->data[i]; }

FixedArr<I64, 8> xs;
FAInit<I64, 8>(&xs);
I64 i;
for (i = 0; i < 5; i++) FAPush<I64, 8>(&xs, i*i);
"sizeof=%d len=%d\n", sizeof(FixedArr<I64,8>), xs.len;
"0=%d 4=%d\n", FAAt<I64,8>(&xs,0), FAAt<I64,8>(&xs,4);
