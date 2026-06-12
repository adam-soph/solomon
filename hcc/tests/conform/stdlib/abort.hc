#include <stdio.hh>
#include <stdlib.hh>
U0 H() { "handler must not run\n"; }
AtExit(&H);
"before abort\n";
Abort;
