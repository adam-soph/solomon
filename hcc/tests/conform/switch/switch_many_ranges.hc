// Many small ranges in one switch.

#include <stdio.hh>
#include <stdlib.hh>
U0 Band(I64 n)
{
  switch (n) {
    case 0 ... 9:   "0s\n";  break;
    case 10 ... 19: "10s\n"; break;
    case 20 ... 29: "20s\n"; break;
    case 30 ... 39: "30s\n"; break;
    case 40 ... 49: "40s\n"; break;
    default:        "50+\n";
  }
}
Band(5);
Band(15);
Band(25);
Band(35);
Band(45);
Band(55);
