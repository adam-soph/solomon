// fs_catch_except_flag.hc — Fs->catch_except is 1 inside catch, 0 outside

#include <stdio.hh>
"before=%d\n", Fs->catch_except;
try {
  throw(99);
} catch {
  "inside=%d val=%d\n", Fs->catch_except, Fs->except_ch;
}
"after=%d\n", Fs->catch_except;
