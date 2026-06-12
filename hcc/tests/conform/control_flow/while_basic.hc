// while loop basics.

#include <stdio.hh>
I64 i = 0;
while (i < 5) {
  "%d\n", i;
  i++;
}

I64 n = 1, prod = 1;
while (n <= 6) {
  prod *= n;
  n++;
}
"%d\n", prod;
