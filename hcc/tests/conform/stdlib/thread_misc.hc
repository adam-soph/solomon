// The non-blocking thread ops are deterministic even single-threaded.
#include <stdio.hh>
#include <threads.hh>
#include <unistd.hh>
"%d\n", ThreadYield();
"%d\n", Gettid() > 0;
"%d\n", Gettid() == Gettid();
I64 Body(I64 x) { return x + 1; }
I64 h = Thread(&Body, 1);
"%d\n", ThreadDetach(h);   // 0; the handle is simply never joined
