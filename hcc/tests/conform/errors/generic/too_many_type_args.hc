//@ error: expects 1 argument(s), got 2
#include <stdlib.hh>
class Box<type T> { T v; };

U0 Main()
{
  Box<I64, I64> b;
}
