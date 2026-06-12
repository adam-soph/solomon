// Fall-through: no break between cases.

#include <stdio.hh>
I64 x = 2;
switch (x) {
  case 1:
    "one ";
  case 2:
    "two ";
  case 3:
    "three\n";
    break;
  case 4:
    "four\n";
}
