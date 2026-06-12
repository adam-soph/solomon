// throw_two_handlers.hc — two separate try/catch blocks in sequence

#include <stdio.hh>
try {
  throw(10);
} catch {
  "first: %d\n", Fs->except_ch;
}
try {
  throw(20);
} catch {
  "second: %d\n", Fs->except_ch;
}
"done\n";
