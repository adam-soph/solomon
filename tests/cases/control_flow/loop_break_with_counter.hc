// break / continue interacting with a top-level (promoted) loop counter, whose final value
// after the break is observed.
I64 i, found = -1, skipped = 0;
for (i = 0; i < 100; i++) {
  if (i % 7 == 0) { skipped++; continue; }
  if (i * i > 500) { found = i; break; }
}
"found=%d skipped=%d i=%d\n", found, skipped, i;
