// throw_from_fn_in_loop.hc — loop calling a function that throws
U0 Validate(I64 x) {
  if (x < 0) throw(x);
  "valid %d\n", x;
}
I64 data[4];
data[0] = 3; data[1] = -1; data[2] = 7; data[3] = -5;
I64 i;
for (i = 0; i < 4; i++) {
  try {
    Validate(data[i]);
  } catch {
    "rejected %d\n", Fs->except_ch;
  }
}
