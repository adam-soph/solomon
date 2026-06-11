//@ error: no field `nope` on type `A`
class A { I64 x; };

U0 Main()
{
  A a;
  a.nope;
}
