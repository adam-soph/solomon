#include <stdlib.hc>
// Div returns a (I64, I64) tuple; unpack with := in a function scope
U0 Main() {
    q, r := Div(17, 5);
    "%d %d\n", q, r;
    q2, r2 := Div(-7, 2);
    "%d %d\n", q2, r2;
}
Main;
