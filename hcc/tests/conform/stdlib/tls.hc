// Single-threaded TLS: fresh keys are distinct, unset reads 0, set/overwrite work.
#include <stdio.hh>
#include <threads.hh>
I64 k1 = TssCreate(), k2 = TssCreate();
"%d\n", k1 != k2;
"%d\n", TssGet(k1);
TssSet(k1, 41);
TssSet(k2, 99);
TssSet(k1, 42);
"%d %d\n", TssGet(k1), TssGet(k2);
