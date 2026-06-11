// throw_in_loop.hc — throw inside a loop exits the loop and the try block
try {
  I64 i;
  for (i = 0; i < 10; i++) {
    if (i == 5) throw(i);
    "iter %d\n", i;
  }
} catch {
  "stopped at %d\n", Fs->except_ch;
}
