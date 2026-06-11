// fixed_arr_str_4.hc — FixedArr<U8*,4>: string elements
class FixedArr<type T, int N> { T data[N]; I64 len; };
U0 FAInit<type T, int N>(FixedArr<T,N> *a) { a->len = 0; }
U0 FAPush<type T, int N>(FixedArr<T,N> *a, T x) { a->data[a->len++] = x; }
T FAAt<type T, int N>(FixedArr<T,N> *a, I64 i) { return a->data[i]; }

FixedArr<U8 *, 4> ss;
FAInit<U8 *, 4>(&ss);
FAPush<U8 *, 4>(&ss, "apple");
FAPush<U8 *, 4>(&ss, "banana");
FAPush<U8 *, 4>(&ss, "cherry");
"%s %s %s len=%d\n", FAAt<U8*,4>(&ss,0), FAAt<U8*,4>(&ss,1), FAAt<U8*,4>(&ss,2), ss.len;
