// Early return via if.
I64 Abs(I64 x)
{
  if (x >= 0)
    return x;
  return -x;
}
"%d %d %d\n", Abs(-5), Abs(0), Abs(7);
