//@ error: call to undeclared function
// Unknown calls are a hard error — there is no implicit-extern fallback.
U0 Main()
{
  NoSuchFn();
}
