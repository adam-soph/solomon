// for loop with multiple init and step expressions via comma-like sequencing.

#include <stdio.hh>
I64 sum = 0;
I64 i;
for (i = 1; i <= 10; i++) {
  sum += i;
}
"sum=%d\n", sum;

// Counting down.
I64 j;
for (j = 5; j > 0; j--) {
  "%d ", j;
}
"\n";
