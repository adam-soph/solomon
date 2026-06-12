// ThreadExit from inside a thread body becomes the Join value (as if returned);
// a normal return is unaffected; from the main flow it ends the program with the
// given status (asserted by the native test legs).
#include <stdio.hh>
#include <threads.hh>
I64 Early(I64 x)
{
  if (x > 10) ThreadExit(x * 2);
  return x;
}
I64 h1 = Thread(&Early, 50);
I64 h2 = Thread(&Early, 5);
"j1=%d\n", Join(h1);
"j2=%d\n", Join(h2);
"done\n";
ThreadExit(9);
"unreachable\n";
