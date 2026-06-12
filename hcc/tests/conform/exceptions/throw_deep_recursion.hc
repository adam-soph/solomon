// throw_deep_recursion.hc — throw from deep recursion caught at top

#include <stdio.hh>
#include <stdlib.hh>
U0 Recurse(I64 depth) {
  if (depth == 0) throw(999);
  Recurse(depth - 1);
}
try {
  Recurse(10);
} catch {
  "caught from depth %d\n", Fs->except_ch;
}
"done\n";
