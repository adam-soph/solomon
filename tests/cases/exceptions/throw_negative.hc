// throw_negative.hc — throw a negative value
try {
  throw(-7);
} catch {
  "caught %d\n", Fs->except_ch;
}
