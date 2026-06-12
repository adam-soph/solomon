// Timed lock/wait, single-threaded: free acquires instantly; held (a self-deadlock)
// times out; an unsignaled condition wait times out. Identical on the interpreter
// (immediate timeout) and native (a real ~20ms kernel wait).
#include <stdio.hh>
#include <threads.hh>
Mutex m;
MutexInit(&m);
"%d\n", MutexTimedLock(&m, 20000000);
"%d\n", MutexTimedLock(&m, 20000000);
MutexUnlock(&m);
Cond cv;
CondInit(&cv);
MutexLock(&m);
"%d\n", CondTimedWait(&cv, &m, 20000000);
MutexUnlock(&m);
"%d\n", MutexTimedLock(&m, 20000000);  // released above: acquires again
