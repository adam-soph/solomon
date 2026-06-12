// Nested switch statements.

#include <stdio.hh>
I64 outer = 1, inner = 2;
switch (outer) {
  case 1:
    switch (inner) {
      case 1: "1,1\n"; break;
      case 2: "1,2\n"; break;
      default: "1,other\n";
    }
    break;
  case 2:
    switch (inner) {
      case 1: "2,1\n"; break;
      default: "2,other\n";
    }
    break;
  default: "other\n";
}
