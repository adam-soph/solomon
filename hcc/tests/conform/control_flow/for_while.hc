// for and while loops with break/continue.

#include <stdio.hh>
I64 i, sum = 0;
for (i = 0; i < 10; i++) {
  if (i == 3)
    continue;
  if (i == 8)
    break;
  sum += i;
}
"sum=%d\n", sum;

I64 k = 1, fact = 1;
while (k <= 5) {
  fact *= k;
  k++;
}
"5!=%d\n", fact;
