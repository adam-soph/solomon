// Reading a nonexistent path fails: the helper returns a negative -errno. ENOENT is 2
// on both Linux and macOS, and the interpreter and the Darwin backend both surface the
// real errno, so the number is identical across those targets. Windows reports a
// different code (ERROR_PATH_NOT_FOUND = 3), so the value check is Unix-only.
#include <errno.hh>
#include <fcntl.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  I64 fd = Open("/no/such/hcc/path", O_RDONLY, 0);
  if (fd < 0) "error: errno=%d\n", -fd;
  else "unexpected success\n";
}
Main;
