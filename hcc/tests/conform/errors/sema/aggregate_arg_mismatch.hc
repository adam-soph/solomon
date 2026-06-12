//@ error: cannot pass `B` to a parameter of type `A`
#include <stdlib.hh>
class A { I64 x; };
class B { I64 x; };

U0 F(A a) {}

U0 Main()
{
  B b;
  F(b);
}
