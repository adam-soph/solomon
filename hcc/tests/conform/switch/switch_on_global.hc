// switch on a global variable.

#include <stdio.hh>
I64 gMode = 2;
switch (gMode) {
  case 0: "off\n"; break;
  case 1: "on\n"; break;
  case 2: "standby\n"; break;
  default: "unknown\n";
}
