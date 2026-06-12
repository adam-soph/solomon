
#include <stdio.hh>
#include <stdlib.hh>
class Buf<type T, int N> { T data[N]; }
U0 Main() {
    Buf<I64, 4> b;
    I64 i;
    for (i = 0; i < 4; i++) b.data[i] = i * 10;
    I64 s = 0;
    for (i = 0; i < 4; i++) s += b.data[i];
    "sum=%d size=%d\n", s, sizeof(Buf<I64, 4>);
}
Main;
