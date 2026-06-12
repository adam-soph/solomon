#include <stdio.hh>
#include <stdlib.hh>
"before\n";
"rc0=%d\n", System("echo from-child");
"rc42=%d\n", System("exit 42");
"rc127=%d\n", System("hcc-no-such-cmd-xyz 2>/dev/null");
"after\n";
