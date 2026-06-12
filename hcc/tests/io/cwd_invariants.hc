// Read-only invariants: Getcwd succeeds into an absolute path, and a bad Chdir fails.
// There's no successful Chdir, so the interpreter's process cwd is not mutated, making
// this race-free under parallel tests. Used only by the Unix-only invariants test (the
// '/'-prefix check).

#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main() {
  U8 buf[256];
  I64 r = Getcwd(buf, 256);
  I64 abs = buf[0] == '/';
  I64 bad = Chdir("/no/such/hcc/dir/xyz") < 0;
  "getcwd=%d abs=%d badchdir=%d\n", r, abs, bad;
}
Main;
