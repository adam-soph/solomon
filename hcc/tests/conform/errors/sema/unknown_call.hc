//@ error: call to undeclared function
// Unknown calls are a hard error — there is no implicit-extern fallback.
#include <stdlib.hh>
U0 Main()
{
  NoSuchFn();
}
