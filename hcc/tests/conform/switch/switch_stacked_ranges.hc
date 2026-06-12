// Multiple stacked ranges in one switch.

#include <stdio.hh>
#include <stdlib.hh>
U0 Grade(I64 score)
{
  switch (score) {
    case 90 ... 100: "A\n"; break;
    case 80 ... 89:  "B\n"; break;
    case 70 ... 79:  "C\n"; break;
    case 60 ... 69:  "D\n"; break;
    case 0 ... 59:   "F\n"; break;
    default:         "?\n";
  }
}
Grade(95);
Grade(83);
Grade(72);
Grade(64);
Grade(45);
Grade(101);
