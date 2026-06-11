// Nested while and do-while.
I64 i = 1;
while (i <= 3) {
  I64 j = 0;
  do {
    "%d%d ", i, j;
    j++;
  } while (j < 2);
  "\n";
  i++;
}
