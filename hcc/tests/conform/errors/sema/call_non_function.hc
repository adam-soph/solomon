//@ error: called value is not a function
#include <stdlib.hh>
U0 Main()
{
  I64 x = 5;
  x();
}
