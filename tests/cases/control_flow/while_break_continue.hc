// while with both break and continue.
I64 i = 0, sum = 0;
while (i < 20) {
  i++;
  if (i % 3 == 0)
    continue;
  if (i > 10)
    break;
  sum += i;
}
"sum=%d i=%d\n", sum, i;
