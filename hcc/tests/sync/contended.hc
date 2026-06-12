// The *blocking* mutex under real contention: 4 threads increment a shared counter in a
// critical section, so the futex wait/wake path runs. Verified on the native runners —
// arm64 Darwin, and the freestanding Linux path on a native linux/aarch64 or
// linux/x86_64 host (e.g. CI).
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
Mutex mu;
I64 mcount = 0;
I64 MWorker(I64 n) { I64 i; for (i = 0; i < 2000; i++) { MutexLock(&mu); mcount++; MutexUnlock(&mu); } return 0; }
U0 Main() {
  MutexInit(&mu);
  I64 h[4]; I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&MWorker, i);
  for (i = 0; i < 4; i++) Join(h[i]);
  "mcount=%d\n", mcount;
}
Main;
