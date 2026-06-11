// return from inside a switch case.
I64 Describe(I64 x)
{
  switch (x) {
    case 0: return 0;
    case 1: return 1;
    default: return -1;
  }
}
"%d %d %d\n", Describe(0), Describe(1), Describe(99);
