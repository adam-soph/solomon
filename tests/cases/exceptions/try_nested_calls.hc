// try_nested_calls.hc — throw through two levels of call
U0 Level2(I64 x) { if (x > 10) throw(x * 2); }
U0 Level1(I64 x) { Level2(x); "level1 ok %d\n", x; }
try {
  Level1(5);
  Level1(15);
} catch {
  "caught %d\n", Fs->except_ch;
}
"done\n";
