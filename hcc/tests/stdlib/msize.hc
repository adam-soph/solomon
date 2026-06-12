#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  U8 *p = MAlloc(40); "%d ", MSize(p);
  U8 *q = MAlloc(7);  "%d ", MSize(q);
  "%d\n", MSize(NULL);
  StrCpy(p, "ok"); "%s\n", p;       // contents unaffected by the header
  U8 *r = MAlloc(16); "%d ", MSize(r);
  r = ReAlloc(r, 16, 80); "%d\n", MSize(r);
}
Main;
