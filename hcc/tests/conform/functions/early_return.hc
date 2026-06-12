// Early return from various paths.

#include <stdio.hh>
I64 Sign(I64 x)
{
  if (x > 0) return 1;
  if (x < 0) return -1;
  return 0;
}
"%d %d %d\n", Sign(-100), Sign(0), Sign(100);
