#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  U8 *p = ReAlloc(NULL, 0, 8);   // == MAlloc(8)
  StrCpy(p, "abcdef");
  p = ReAlloc(p, 8, 64);          // grow
  "%s\n", p;
  p = ReAlloc(p, 64, 4);          // shrink (keeps first 4)
  "%c%c%c%c\n", p[0], p[1], p[2], p[3];
}
Main;
