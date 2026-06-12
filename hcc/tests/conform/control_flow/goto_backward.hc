// goto backward (simple loop via goto).

#include <stdio.hh>
I64 n = 0;
loop:
if (n < 5) {
  "%d ", n;
  n++;
  goto loop;
}
"\n";
