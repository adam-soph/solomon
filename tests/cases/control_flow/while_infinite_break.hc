// Infinite while(1) with a break condition.
I64 x = 1;
while (1) {
  x *= 2;
  if (x > 100)
    break;
}
"%d\n", x;
