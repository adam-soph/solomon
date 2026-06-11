// continue in inner loop of nested for.
I64 i, j;
for (i = 0; i < 3; i++) {
  for (j = 0; j < 5; j++) {
    if (j % 2 == 0)
      continue;
    "%d%d ", i, j;
  }
  "\n";
}
