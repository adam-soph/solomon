//@ error: cannot assign `B` to `A`
// Aggregates are nominal: two differently-named classes never assign across each other,
// even with identical fields.
#include <stdlib.hh>
class A { I64 x; };
class B { I64 x; };

U0 Main()
{
  A a;
  B b;
  a = b;
}
