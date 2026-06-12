// try_no_throw.hc — try with no throw: catch body not entered

#include <stdio.hh>
"before\n";
try {
  "in try\n";
  I64 x = 2 + 2;
  "x=%d\n", x;
} catch {
  "catch unreached\n";
}
"after\n";
