#include <stdio.hh>
#include <stdlib.hh>
U0 H1() { "h1\n"; }
U0 H2() { "h2\n"; }
AtExit(&H1);
AtExit(&H2);
"registered\n";
Exit(0);
"unreachable\n";
