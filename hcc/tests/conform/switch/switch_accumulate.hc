// Accumulate by switch category.

#include <stdio.hh>
I64 vals[10];
vals[0]=1; vals[1]=5; vals[2]=12; vals[3]=3; vals[4]=8;
vals[5]=15; vals[6]=2; vals[7]=7; vals[8]=20; vals[9]=6;

I64 small=0, med=0, large=0, i;
for (i = 0; i < 10; i++) {
  switch (vals[i]) {
    case 1 ... 5:   small++; break;
    case 6 ... 10:  med++;   break;
    case 11 ... 20: large++; break;
  }
}
"small=%d med=%d large=%d\n", small, med, large;
