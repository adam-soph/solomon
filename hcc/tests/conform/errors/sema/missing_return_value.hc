//@ error: missing return value in non-void function
#include <stdlib.hh>
I64 F()
{
  return;
}

U0 Main()
{
  F();
}
