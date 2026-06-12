// throw_in_switch.hc — throw from inside a switch case

#include <stdio.hh>
#include <stdlib.hh>
U0 Process(I64 code) {
  switch (code) {
    case 1: "ok\n"; break;
    case 2: throw(code); break;
    default: "default\n";
  }
}
try {
  Process(1);
  Process(2);
  Process(3);
} catch {
  "caught code=%d\n", Fs->except_ch;
}
