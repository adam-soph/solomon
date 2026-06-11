// switch on the result of a function call.
I64 Cat(I64 x) { return x % 4; }
I64 i;
for (i = 0; i < 8; i++) {
  switch (Cat(i)) {
    case 0: "Q "; break;
    case 1: "W "; break;
    case 2: "E "; break;
    case 3: "R "; break;
  }
}
"\n";
