// break exits only the innermost loop.
I64 i, j;
for (i = 0; i < 3; i++) {
  for (j = 0; j < 10; j++) {
    if (j == 3)
      break;
  }
  "i=%d j=%d\n", i, j;
}
