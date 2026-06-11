// Boolean results of pointer comparisons: <, >, <=, >=.
I64 arr[4] = {10, 20, 30, 40};
I64 *p = arr + 1;
I64 *q = arr + 3;
"%d %d %d %d\n", p < q, q > p, p <= p, q >= arr;
