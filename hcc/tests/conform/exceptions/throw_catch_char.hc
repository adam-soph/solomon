// throw_catch_char.hc — throw a char code and catch it

#include <math.hh>
#include <stdio.hh>
try {
  throw('E');
} catch {
  "caught %d\n", Fs->except_ch;
}
"after\n";
