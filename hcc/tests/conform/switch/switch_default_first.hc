// default case appearing before other cases.

#include <stdio.hh>
I64 v = 7;
switch (v) {
  default: "default\n"; break;
  case 1: "one\n"; break;
  case 2: "two\n"; break;
}
