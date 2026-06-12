// type_switch_format.hc — switch type with different format per T

#include <stdio.hh>
#include <stdlib.hh>
U0 Print<type T>(T x) {
  switch type (T) {
    case I64:  "%d\n", x; return;
    case F64:  "%.4f\n", x; return;
    default:   "?\n"; return;
  }
}
Print(42);
Print(-1);
Print(3.1415);
Print(0.0);
