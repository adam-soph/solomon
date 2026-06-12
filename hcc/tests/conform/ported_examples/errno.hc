// errno.hc — C `<errno.h>`: named error codes plus StrError/Perror over the stdlib's
// `-errno` returns. Opening a missing file yields `-ENOENT`; StrError turns any
// `-errno` into a message. The codes are Linux-canonical on every target, so
// `-fd == ENOENT` and the rendered text are identical under the interpreter and the
// native backends.



#include <errno.hh>
#include <fcntl.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main()
{
  I64 fd = Open("hcc_no_such_file_4c1f.txt", O_RDONLY, 0);
  if (fd < 0) {
    Print("open: %s (errno %d)\n", StrError(fd), -fd);
    if (-fd == ENOENT) Print("recognized ENOENT\n");
  } else {
    Close(fd);
  }

  // StrError over a few codes directly (pure — identical on every target).
  Print("%d=%s\n", EINVAL, StrError(EINVAL));
  Print("%d=%s\n", ENOSPC, StrError(ENOSPC));
  Print("%d=%s\n", EACCES, StrError(EACCES));
}

Main;
