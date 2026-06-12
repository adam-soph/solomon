// Condition variable: 4 workers block in `CondWait` until the main thread sets the
// predicate and `CondBroadcast`s, then each bumps `done`. The synchronous interpreter
// can't model a consumer that waits for a later producer, so this is native-only.
#include <stdatomic.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
#include <time.hh>
Mutex mu;
Cond cv;
I64 go = 0;
I64 done = 0;
I64 W(I64 n) {
  MutexLock(&mu);
  while (!go) CondWait(&cv, &mu);
  MutexUnlock(&mu);
  AtomicAdd(&done, 1);
  return 0;
}
U0 Main() {
  MutexInit(&mu); CondInit(&cv);
  I64 h[4]; I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&W, i);
  Sleep(50000000);  // 50ms: let the workers reach CondWait
  MutexLock(&mu); go = 1; CondBroadcast(&cv); MutexUnlock(&mu);
  for (i = 0; i < 4; i++) Join(h[i]);
  "done=%d\n", done;
}
Main;
