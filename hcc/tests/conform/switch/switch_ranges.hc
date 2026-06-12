// switch with case lo ... hi range syntax.

#include <stdio.hh>
#include <stdlib.hh>
U0 Classify(I64 v)
{
  switch (v) {
    case 0: "zero\n"; break;
    case 1 ... 3: "small\n"; break;
    case 4 ... 9: "medium\n"; break;
    case 10 ... 99: "large\n"; break;
    default: "huge\n";
  }
}
Classify(0);
Classify(2);
Classify(7);
Classify(50);
Classify(100);
