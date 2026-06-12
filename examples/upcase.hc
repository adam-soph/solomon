// upcase.hc — a stdin filter: read each line and echo it upper-cased. Demonstrates the
// text-input family (ReadLine over the `Read` primitive). With no input it reads EOF
// immediately and prints nothing, so it is deterministic under the test harness (which
// runs examples with an empty stdin), while still doing real work when piped input.



#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <unistd.hh>
U0 Main()
{
  U8 *line;
  while ((line = ReadLine(STDIN))) {
    StrToUpper(line);
    "%s\n", line;
    Free(line);
  }
}

Main;
