//@ error: expects 1 argument(s), got 2
#include <stdlib.hh>
U0 F(I64 a) {}

U0 Main()
{
  F(1, 2);
}
