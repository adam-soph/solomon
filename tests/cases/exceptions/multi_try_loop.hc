// multi_try_loop.hc — multiple sequential try/catch validating inputs
U0 Check(I64 x) {
  if (x < 0) throw(-1);
  if (x > 100) throw(1);
  "ok %d\n", x;
}
I64 vals[5];
vals[0] = 50; vals[1] = -3; vals[2] = 200; vals[3] = 0; vals[4] = 75;
I64 i;
for (i = 0; i < 5; i++) {
  try {
    Check(vals[i]);
  } catch {
    "bad %d code=%d\n", vals[i], Fs->except_ch;
  }
}
