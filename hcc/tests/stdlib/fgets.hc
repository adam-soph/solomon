#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <unistd.hh>
U0 Main() {
  U8 line[32];
  I64 n = 0;
  while (FGetS(line, 32, STDIN)) {
    I64 len = StrLen(line);
    if (len > 0 && line[len - 1] == '\n') line[len - 1] = 0;
    "[%s]\n", line;
    n++;
  }
  "lines=%d\n", n;
}
Main;
