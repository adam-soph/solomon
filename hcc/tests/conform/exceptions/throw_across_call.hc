// throw_across_call.hc — throw unwinds through a function call

#include <stdio.hh>
#include <stdlib.hh>
U0 Inner() { throw(42); }
U0 Outer() { Inner(); "unreached\n"; }
try {
  Outer();
} catch {
  "caught %d\n", Fs->except_ch;
}
"done\n";
