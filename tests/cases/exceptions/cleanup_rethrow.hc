// cleanup_rethrow.hc — finally-style cleanup-before-rethrow pattern
I64 cleaned = 0;
U0 DoWork() {
  try {
    throw(42);
  } catch {
    cleaned = 1;
    throw;
  }
}
try {
  DoWork();
} catch {
  "cleaned=%d val=%d\n", cleaned, Fs->except_ch;
}
