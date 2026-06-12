// Nested switch inside a function, both branches exercised.

#include <stdio.hh>
#include <stdlib.hh>
U0 Classify(I64 a, I64 b)
{
  switch (a) {
    case 0:
      switch (b) {
        case 0: "0,0\n"; break;
        default: "0,n\n";
      }
      break;
    default:
      switch (b) {
        case 0: "n,0\n"; break;
        default: "n,n\n";
      }
  }
}
Classify(0, 0);
Classify(0, 5);
Classify(3, 0);
Classify(3, 5);
