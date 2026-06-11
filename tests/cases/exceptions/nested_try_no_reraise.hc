// nested_try_no_reraise.hc — nested try, inner catches without re-raising
try {
  try {
    throw(55);
  } catch {
    "inner caught %d\n", Fs->except_ch;
    // does not re-raise
  }
  "after inner try\n";
} catch {
  "outer catch unreached\n";
}
"done\n";
