// switch [x] bracket form in a loop.
I64 i;
for (i = 0; i < 4; i++) {
  switch [i] {
    case 0: "zero\n"; break;
    case 1: "one\n"; break;
    case 2: "two\n"; break;
    default: "three+\n";
  }
}
