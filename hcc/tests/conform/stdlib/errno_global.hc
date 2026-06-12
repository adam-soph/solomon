
// The implicit errno: zero at start, set by a failing primitive (positive code,
// matching the negative return), assignable, and untouched by a successful call.
#include <errno.hh>
#include <fcntl.hh>
#include <stdio.hh>
"%d\n", errno;
I64 fd = Open("/hcc/definitely/not/here", O_RDONLY, 0);
"%d %d\n", fd == -ENOENT, errno == ENOENT;
"%d\n", Errno() == ENOENT;
"%s\n", StrError(errno);
errno = 0;
"%d\n", errno;
I64 r = Rename("/hcc/no/such/src", "/hcc/no/such/dst");  // fails: errno == -r
"%d\n", r < 0 && errno == -r;
errno = 7;
errno++;                                                  // it's a plain lvalue
"%d\n", errno;
