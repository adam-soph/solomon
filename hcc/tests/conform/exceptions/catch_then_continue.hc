// catch_then_continue.hc — after catch, execution continues normally

#include <stdio.hh>
I64 total = 0;
I64 i;
for (i = 1; i <= 5; i++) {
  try {
    if (i == 3) throw(300);
    total += i;
  } catch {
    total += Fs->except_ch;
  }
}
"total=%d\n", total;
