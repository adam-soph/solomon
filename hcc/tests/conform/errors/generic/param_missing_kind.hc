//@ error: generic parameter must be declared with
// A bare `<T>` is a parse error: every generic parameter needs a kind keyword
// (`type`, `comparable`, or `int`).
#include <stdlib.hh>
class Box<T> { T v; };

U0 Main() {}
