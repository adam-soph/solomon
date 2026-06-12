// throw_updates_global.hc — side effects before throw are visible in catch

#include <stdio.hh>
#include <stdlib.hh>
I64 counter = 0;
U0 Inc() { counter++; throw(counter); }
try { Inc(); } catch { "caught counter=%d\n", counter; }
try { Inc(); } catch { "caught counter=%d\n", counter; }
