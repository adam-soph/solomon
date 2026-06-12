// ExitRaw (C _Exit) and Abort skip the handlers. Abort's "Aborted" goes to stderr,
// so stdout holds only the pre-abort line.
#include <stdio.hh>
#include <stdlib.hh>
U0 H() { "handler must not run\n"; }
AtExit(&H);
"before\n";
ExitRaw(0);
