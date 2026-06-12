//@ error: cannot dereference a non-pointer
#include <stdlib.hh>
U0 Main()
{
  I64 x = 5;
  *x;
}
