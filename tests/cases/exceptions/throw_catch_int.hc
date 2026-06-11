// throw_catch_int.hc — throw an integer total
I64 SumUntil(I64 n, I64 cap) {
  I64 i, total = 0;
  for (i = 1; i <= n; i++) {
    total += i;
    if (total > cap) throw(total);
  }
  return total;
}
try {
  I64 s = SumUntil(100, 20);
  "unreached sum=%d\n", s;
} catch {
  "capped at %d\n", Fs->except_ch;
}
