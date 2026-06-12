// switch in a loop accumulating a weighted sum.

#include <stdio.hh>
I64 data[6];
data[0] = 0; data[1] = 1; data[2] = 2;
data[3] = 3; data[4] = 0; data[5] = 1;

I64 sum = 0, i;
for (i = 0; i < 6; i++) {
  switch (data[i]) {
    case 0: sum += 1; break;
    case 1: sum += 10; break;
    case 2: sum += 100; break;
    case 3: sum += 1000; break;
  }
}
"sum=%d\n", sum;
