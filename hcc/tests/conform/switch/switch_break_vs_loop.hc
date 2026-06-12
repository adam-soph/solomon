// Confirm break in switch does NOT exit the enclosing loop.

#include <stdio.hh>
I64 i, count = 0;
for (i = 0; i < 10; i++) {
  switch (i % 3) {
    case 0: count += 1; break;
    case 1: count += 2; break;
    case 2: count += 4; break;
  }
}
"count=%d\n", count;
