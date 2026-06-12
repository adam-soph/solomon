// throw_zero.hc — throw 0 (the false-y value)

#include <stdio.hh>
try {
  throw(0);
} catch {
  "caught %d\n", Fs->except_ch;
}
