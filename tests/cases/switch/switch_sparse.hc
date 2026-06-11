// Sparse switch (large gaps between case values).
I64 v = 1000;
switch (v) {
  case 1: "one\n"; break;
  case 100: "hundred\n"; break;
  case 1000: "thousand\n"; break;
  case 10000: "ten-thousand\n"; break;
  default: "other\n";
}
