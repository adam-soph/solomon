#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  I64 c, n = 0;
  while ((c = GetChar()) >= 0) n++;
  "bytes=%d\n", n;
}
Main;
