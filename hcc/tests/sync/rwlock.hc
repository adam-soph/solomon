// Reader/writer lock: 4 threads each do 1000 iterations of (write-locked increment,
// read-locked read). The writes are mutually exclusive, so the counter is exactly 4000.
// This is interleaving-independent, so the synchronous interpreter handles it too.
#include <stdio.hh>
#include <stdlib.hh>
#include <threads.hh>
RwLock rw;
I64 counter = 0;
I64 W(I64 n) {
  I64 i;
  for (i = 0; i < 1000; i++) {
    RwLockWLock(&rw); counter++; RwLockWUnlock(&rw);
    RwLockRLock(&rw); I64 t = counter; RwLockRUnlock(&rw);
  }
  return 0;
}
U0 Main() {
  RwLockInit(&rw);
  I64 h[4]; I64 i;
  for (i = 0; i < 4; i++) h[i] = Thread(&W, i);
  for (i = 0; i < 4; i++) Join(h[i]);
  "counter=%d\n", counter;
}
Main;
