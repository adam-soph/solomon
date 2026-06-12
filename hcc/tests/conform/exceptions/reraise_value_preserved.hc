// reraise_value_preserved.hc — re-raised exception preserves the value

#include <stdio.hh>
#include <stdlib.hh>
U0 Inner() { throw(77); }
try {
  try {
    Inner();
  } catch {
    I64 v = Fs->except_ch;
    throw;
  }
} catch {
  "outer got %d\n", Fs->except_ch;
}
