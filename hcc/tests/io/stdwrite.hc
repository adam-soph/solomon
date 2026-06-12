// Writes a line to stderr (fd 2, a side channel), then a line to stdout (fd 1), and
// prints the byte count StdWrite returned. The deterministic *stdout* is
// "stdout line\nwrote=12\n". The stderr write must NOT appear there.
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <unistd.hh>
U0 Main() {
  U8 *o = "stdout line\n";
  U8 *e = "stderr line\n";
  StdWrite(STDERR, e, StrLen(e)); // → fd 2, not captured in stdout
  I64 w = StdWrite(STDOUT, o, StrLen(o));
  "wrote=%d\n", w;
}
Main;
