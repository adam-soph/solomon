// throw_negative.hc — throw a negative value

#include <stdio.hh>
try {
  throw(-7);
} catch {
  "caught %d\n", Fs->except_ch;
}
