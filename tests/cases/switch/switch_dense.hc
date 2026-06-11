// Dense all-constant switch (exercises jump table in arm64).
U0 Name(I64 n)
{
  switch (n) {
    case 0: "zero\n"; break;
    case 1: "one\n"; break;
    case 2: "two\n"; break;
    case 3: "three\n"; break;
    case 4: "four\n"; break;
    case 5: "five\n"; break;
    case 6: "six\n"; break;
    case 7: "seven\n"; break;
    case 8: "eight\n"; break;
    case 9: "nine\n"; break;
    default: "many\n";
  }
}
I64 i;
for (i = 0; i <= 10; i++)
  Name(i);
