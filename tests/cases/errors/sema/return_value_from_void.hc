//@ error: returning a value from a U0
U0 F()
{
  return 5;
}

U0 Main()
{
  F();
}
