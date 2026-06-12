// goto inside a function.

#include <stdio.hh>
#include <stdlib.hh>
U0 Search(I64 target)
{
  I64 i;
  for (i = 0; i < 10; i++) {
    if (i == target)
      goto found;
  }
  "not found\n";
  return;
found:
  "found at %d\n", i;
}
Search(4);
Search(15);
