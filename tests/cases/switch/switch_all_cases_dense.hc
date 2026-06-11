// Dense 0..15 switch — every case matched in a loop.
U0 Hex(I64 n)
{
  switch (n) {
    case 0: "0"; break;
    case 1: "1"; break;
    case 2: "2"; break;
    case 3: "3"; break;
    case 4: "4"; break;
    case 5: "5"; break;
    case 6: "6"; break;
    case 7: "7"; break;
    case 8: "8"; break;
    case 9: "9"; break;
    case 10: "a"; break;
    case 11: "b"; break;
    case 12: "c"; break;
    case 13: "d"; break;
    case 14: "e"; break;
    case 15: "f"; break;
    default: "?";
  }
}
I64 i;
for (i = 0; i <= 15; i++)
  Hex(i);
"\n";
