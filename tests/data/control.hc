// control.hc — switch/case ranges, do-while, ternary, bit ops, casts, sizeof,
// goto/labels.
U0 Classify(I64 v)
{
  switch (v) {
    case 0:
      "zero\n";
      break;
    case 1 ... 3:
      "small\n";
      break;
    default:
      "other\n";
  }

  I64 i = 0;
  do {
    i++;
  } while (i < 5);

  I64 flags = (v & 0x0F) | 0x10;
  I64 shifted = flags << 2 >> 1;
  Bool ok = v > 0 && v < 100;
  I64 c = ok ? 1 : -1;
  I64 casted = (I64)3.5;
  I64 sz = sizeof(I64);

  if (v < 0)
    goto done;
  "non-negative: %d %d\n", shifted, c;
done:
  return;
}
