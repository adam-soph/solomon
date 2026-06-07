#ifndef _UNISTD_HC
#define _UNISTD_HC
// unistd.hc — C/POSIX `<unistd.h>`: file-descriptor I/O (`Read`/`Write`/`Close`/`LSeek`)
// and process queries (`Getpid`/…/`Chdir`/`Getcwd`). `Mkdir` (POSIX `<sys/stat.h>`) is
// folded in here too, alongside the other directory ops.
//
// These are intrinsics: the prototypes live here, and the compiler lowers them to the
// host's syscalls on the freestanding targets, or to libc on Darwin. `<socket.hc>`
// shares the same `Read`/`Write`/`Close` for sockets. They do real, impure I/O / process
// control, so a program using them is not reproducible; conformance is by property, not
// by interp-vs-native value. Include with `#include <unistd.hc>`.
//
// Errors are returned as the value, Unix-style: a successful call returns a non-negative
// result (a byte count, an offset, 0), a failure returns a negative `-errno`.

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

// The standard stream fds, for `StdWrite`.
#define STDIN   0
#define STDOUT  1
#define STDERR  2

// --- raw fd primitives (intrinsics) ------------------------------------------

public I64 LSeek(I64 fd, I64 off, I64 whence);    // new absolute offset, or -errno
public I64 Read(I64 fd, U8 *buf, I64 n);          // bytes read (0 = EOF), or -errno
public I64 Write(I64 fd, U8 *buf, I64 n);         // bytes written, or -errno
public I64 Close(I64 fd);                         // 0, or -errno

// `StdWrite(fd, buf, n)` writes to a standard stream portably: `fd` 1 (STDOUT) =
// stdout, 2 (STDERR) = stderr. Unlike `Write` above (a POSIX fd op), it also works on
// Windows: it lowers to the write syscall or libc on POSIX, and
// `WriteFile(GetStdHandle(...))` on Windows. The printf machinery is built on it.
public I64 StdWrite(I64 fd, U8 *buf, I64 n);      // bytes written, or -errno

// --- process / user ids (intrinsics) -----------------------------------------

public I64 Getpid();   // the current process id
public I64 Getppid();  // the parent process id
public I64 Getuid();   // the real user id
public I64 Getgid();   // the real group id

// --- working directory & directory mutation (intrinsics) ---------------------
//
// Each returns 0 on success, or a negative `-errno`. `Getcwd` writes the current
// directory's path (NUL-terminated) into `buf` (capacity `size`), `-ERANGE` if too
// small. `Mkdir` is POSIX `<sys/stat.h>`, kept here with the other directory ops.

public I64 Chdir(U8 *path);
public I64 Getcwd(U8 *buf, I64 size);
public I64 Mkdir(U8 *path, I64 mode);

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
