// Create a directory, write a file in it, rename it, read it back, then remove it. The
// missing-source remove yields -ENOENT = -2 on every target. `__DIR__` is substituted
// by the test harness with a process-unique temp directory.
#include <errno.hh>
#include <fcntl.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <unistd.hh>
U0 Main() {
  "mkdir=%d\n", Mkdir("__DIR__", 0700);
  U8 *msg = "hi\n";
  I64 wfd = Open("__DIR__/a.txt", O_WRONLY | O_CREAT | O_TRUNC, MODE_0644);
  "write=%d\n", WriteAll(wfd, msg, StrLen(msg));
  Close(wfd);
  "rename=%d\n", Rename("__DIR__/a.txt", "__DIR__/b.txt");
  U8 buf[16];
  I64 rfd = Open("__DIR__/b.txt", O_RDONLY, 0);
  I64 n = Read(rfd, buf, 16);
  Close(rfd);
  buf[n] = 0;
  "read=%d got=%s", n, buf;
  "rm_missing=%d\n", Remove("__DIR__/a.txt");
  "rm=%d\n", Remove("__DIR__/b.txt");
}
Main;
