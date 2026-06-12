#include <stdio.hh>
#include <unistd.hh>
#include <unistd.hh>   // STDOUT/STDIN/STDERR
// FPrint/FPutC/FPutS take an explicit fd; STDOUT here so the output is captured.
I64 n = FPrint(STDOUT, "x=%d s=%s f=%.3f\n", -7, "hi", 2.5);
"%d\n", n;          // the byte count FPrint returned
"%d\n", FPutC('Q', STDOUT);
FPutC('\n', STDOUT);
"%d\n", FPutS("line\n", STDOUT);
