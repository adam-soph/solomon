// Deterministic value check: Chdir to root, then Getcwd reports exactly "/". Run in an
// isolated child so the cwd change can't leak.

#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main() {
  U8 buf[256];
  "chdir=%d\n", Chdir("/");
  Getcwd(buf, 256);
  "cwd=%s\n", buf;
}
Main;
