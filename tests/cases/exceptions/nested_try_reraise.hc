// nested_try_reraise.hc — nested try with bare throw; re-raise
try {
  try {
    throw(7);
  } catch {
    "inner caught %d\n", Fs->except_ch;
    throw;
  }
} catch {
  "outer caught %d flag=%d\n", Fs->except_ch, Fs->catch_except;
}
"flag now %d\n", Fs->catch_except;
