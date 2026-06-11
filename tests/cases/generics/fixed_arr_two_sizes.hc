// fixed_arr_two_sizes.hc — two distinct N values are independent types
class FixedArr<type T, int N> { T data[N]; I64 len; };
U0 FAInit<type T, int N>(FixedArr<T,N> *a) { a->len = 0; }
U0 FAPush<type T, int N>(FixedArr<T,N> *a, T x) { a->data[a->len++] = x; }

FixedArr<I64, 4> small;
FixedArr<I64, 16> big;
FAInit<I64, 4>(&small);
FAInit<I64, 16>(&big);
FAPush<I64, 4>(&small, 1);
FAPush<I64, 4>(&small, 2);
FAPush<I64, 16>(&big, 100);
FAPush<I64, 16>(&big, 200);
FAPush<I64, 16>(&big, 300);
"small len=%d sizeof=%d\n", small.len, sizeof(FixedArr<I64,4>);
"big len=%d sizeof=%d\n", big.len, sizeof(FixedArr<I64,16>);
