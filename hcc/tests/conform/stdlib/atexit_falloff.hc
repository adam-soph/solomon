// Handlers also run when the top level simply ends (normal termination).
#include <stdio.hh>
#include <stdlib.hh>
U0 H1() { "h1\n"; }
U0 H2() { "h2\n"; }
AtExit(&H1);
AtExit(&H2);
"end of top level\n";
