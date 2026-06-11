// switch inside a loop — break exits switch, not loop.
I64 i;
for (i = 0; i < 5; i++) {
  switch (i) {
    case 0: "zero\n"; break;
    case 1: "one\n"; break;
    case 2: "two\n"; break;
    default: "other\n"; break;
  }
}
"done\n";
