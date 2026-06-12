#ifndef _UNISTD_HC
#define _UNISTD_HC
// unistd.hc — implementation (interface in unistd.hh).

#include <unistd.hh>


// The standard stream fds, for `StdWrite`.

// --- raw fd primitives (intrinsics) ------------------------------------------


// `StdWrite(fd, buf, n)` writes to a standard stream portably: `fd` 1 (STDOUT) =
// stdout, 2 (STDERR) = stderr. Unlike `Write` above (a POSIX fd op), it also works on
// Windows: it lowers to the write syscall or libc on POSIX, and
// `WriteFile(GetStdHandle(...))` on Windows. The printf machinery is built on it.

// --- process / user ids (intrinsics) -----------------------------------------


// --- working directory & directory mutation (intrinsics) ---------------------
//
// Each returns 0 on success, or a negative `-errno`. `Getcwd` writes the current
// directory's path (NUL-terminated) into `buf` (capacity `size`), `-ERANGE` if too
// small. `Mkdir` is POSIX `<sys/stat.h>`, kept here with the other directory ops.


// --- fd helpers --------------------------------------------------------------

// Write all `n` bytes to `fd`, looping over partial writes. Returns 0, or -errno.
public I64 WriteAll(I64 fd, U8 *buf, I64 n)
{
  I64 off = 0;
  while (off < n) {
    I64 w = Write(fd, buf + off, n - off);
    if (w < 0) return w;
    if (w == 0) return -5;  // EIO: no progress
    off += w;
  }
  return 0;
}


#endif
