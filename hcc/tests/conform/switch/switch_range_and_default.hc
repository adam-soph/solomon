// Range cases plus default.

#include <stdio.hh>
#include <stdlib.hh>
U0 Bucket(I64 n)
{
  switch (n) {
    case 1 ... 5:   "low\n";    break;
    case 6 ... 10:  "mid\n";    break;
    case 11 ... 20: "high\n";   break;
    default:        "extreme\n";
  }
}
Bucket(3);
Bucket(8);
Bucket(15);
Bucket(99);
Bucket(0);
