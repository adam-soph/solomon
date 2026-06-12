// Cross-thread TLS: each worker writes its own value under a shared key and returns
// what it reads back — its own write, never another thread's (or main's). Main sets
// the key first; whether main's slot survives differs by engine (the synchronous
// interpreter runs workers on the main tid), so only the workers' round-trips and
// CallOnce's exactly-once guarantee are asserted.
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
I64 once_runs = 0;
U0 OnceBody() { once_runs++; }
Once once;

I64 key;
I64 Worker(I64 v)
{
  CallOnce(&once, &OnceBody);
  TssSet(key, v);
  ThreadYield();
  return TssGet(key);
}

key = TssCreate();
TssSet(key, 7);
I64 h0 = Thread(&Worker, 100);
I64 h1 = Thread(&Worker, 200);
I64 h2 = Thread(&Worker, 300);
"w0=%d\n", Join(h0);
"w1=%d\n", Join(h1);
"w2=%d\n", Join(h2);
"once=%d\n", once_runs;
