// switch with negative range.
U0 Sign(I64 n)
{
  switch (n) {
    case -100 ... -1: "negative\n"; break;
    case 0: "zero\n"; break;
    case 1 ... 100: "positive\n"; break;
    default: "extreme\n";
  }
}
Sign(-50);
Sign(0);
Sign(42);
Sign(200);
