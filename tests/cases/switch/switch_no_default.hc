// switch with no default — unmatched value falls through to after switch.
I64 x = 5;
switch (x) {
  case 1: "one\n"; break;
  case 2: "two\n"; break;
}
"after switch\n";
