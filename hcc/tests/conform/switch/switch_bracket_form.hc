// switch [x] bracket form (parsed identically to switch (x)).

#include <stdio.hh>
I64 v = 3;
switch [v] {
  case 1: "one\n"; break;
  case 2: "two\n"; break;
  case 3: "three\n"; break;
  default: "other\n";
}
