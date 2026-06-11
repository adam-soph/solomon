//@ error: anonymous class/union types are not supported inside a generic
class Box<type T> { class { T v; } inner; };

U0 Main()
{
  Box<I64> b;
}
