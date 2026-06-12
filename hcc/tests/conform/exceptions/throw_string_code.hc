// throw_string_code.hc — multi-char code (HolyC multi-byte char literal)

#include <stdio.hh>
#include <stdlib.hh>
U0 Check(I64 age) {
  if (age < 0)   throw('LOW');
  if (age > 150) throw('HIGH');
  "ok %d\n", age;
}
I64 ages[4];
ages[0] = 30; ages[1] = -5; ages[2] = 200; ages[3] = 42;
I64 i;
for (i = 0; i < 4; i++) {
  try {
    Check(ages[i]);
  } catch {
    "rejected %d code=%d\n", ages[i], Fs->except_ch;
  }
}
