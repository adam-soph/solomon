// tuple_conditional.hc — tuple chosen by conditional and returned
(I64, I64) Either(I64 flag) {
  if (flag) return 1, 2;
  return 3, 4;
}
a, b := Either(1);
"%d %d\n", a, b;
c, d := Either(0);
"%d %d\n", c, d;
