
#include <stdio.hh>
#include <stdlib.hh>
class Pt { I64 x; }
U0 Show<type T>(T v) {
    switch type (T) {
        case I64: "int %d\n", v;
        case F64: "flt %.1f\n", v;
        case Pt:  "pt %d\n", v.x;
        default:  "other\n";
    }
}
U0 Main() {
    Show(7);
    Show(2.5);
    Pt p; p.x = 9; Show(p);
    Show("hi");
}
Main;
