#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
I64 runs = 0;
U0 Body() { runs++; "ran\n"; }
Once o;
OnceInit(&o);
CallOnce(&o, &Body);
CallOnce(&o, &Body);
CallOnce(&o, &Body);
"%d\n", runs;
