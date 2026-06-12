// value_param_as_dim.hc — int N used as array dim and in arithmetic

#include <stdio.hh>
#include <stdlib.hh>
class Matrix<type T, int R, int C> { T data[R * C]; };
I64 MatGet(I64 *data, I64 cols, I64 r, I64 c) { return data[r * cols + c]; }
U0 MatSet(I64 *data, I64 cols, I64 r, I64 c, I64 v) { data[r * cols + c] = v; }

Matrix<I64, 2, 3> m;
MatSet(m.data, 3, 0, 0, 1); MatSet(m.data, 3, 0, 1, 2); MatSet(m.data, 3, 0, 2, 3);
MatSet(m.data, 3, 1, 0, 4); MatSet(m.data, 3, 1, 1, 5); MatSet(m.data, 3, 1, 2, 6);
I64 r, c;
for (r = 0; r < 2; r++) {
  for (c = 0; c < 3; c++) "%d ", MatGet(m.data, 3, r, c);
  "\n";
}
"sizeof=%d\n", sizeof(Matrix<I64, 2, 3>);
