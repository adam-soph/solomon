// Write "hcc\n" to __PATH__, read it back, and print the content and file size.
// The stdout is deterministic: "got: hcc\nsize=4\n". `__PATH__` is substituted by
// the test harness with a process-unique temp path.
#include <fcntl.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <unistd.hh>
U0 Main() {
  U8 *msg = "hcc\n";
  I64 wfd = Open("__PATH__", O_WRONLY | O_CREAT | O_TRUNC, MODE_0644);
  if (wfd < 0) { "write failed: %d\n", -wfd; return; }
  WriteAll(wfd, msg, StrLen(msg));
  Close(wfd);
  U8 buf[64];
  I64 rfd = Open("__PATH__", O_RDONLY, 0);
  if (rfd < 0) { "read failed: %d\n", -rfd; return; }
  I64 n = Read(rfd, buf, 64);
  Close(rfd);
  buf[n] = 0;
  "got: %s", buf;
  "size=%d\n", FileSize("__PATH__");
}
Main;
