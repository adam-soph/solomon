// throw_catch_char.hc — throw a char code and catch it
try {
  throw('E');
} catch {
  "caught %d\n", Fs->except_ch;
}
"after\n";
