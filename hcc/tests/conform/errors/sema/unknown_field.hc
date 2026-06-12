//@ error: no field `nope` on type `A`
#include <stdlib.hh>
class A { I64 x; };

U0 Main()
{
  A a;
  a.nope;
}
