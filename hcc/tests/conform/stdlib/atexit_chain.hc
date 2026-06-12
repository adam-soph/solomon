// A handler may register another handler; the new one still runs (LIFO over the
// growing table), and Exit's status expression is evaluated before the handlers.
#include <stdio.hh>
#include <stdlib.hh>
U0 Late() { "late\n"; }
U0 H() { AtExit(&Late); "h\n"; }
I64 Status() { "status computed\n"; return 0; }
AtExit(&H);
Exit(Status());
