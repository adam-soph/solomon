//@ error: missing return value in non-void function
I64 F()
{
  return;
}

U0 Main()
{
  F();
}
