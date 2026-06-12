#ifndef _UNISTD_HH
#define _UNISTD_HH
// unistd.hh — C/POSIX `<unistd.h>`: file-descriptor I/O (`Read`/`Write`/`Close`/`LSeek`)
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
#define STDIN   0
#define STDOUT  1
#define STDERR  2
public I64 LSeek(I64 fd, I64 off, I64 whence);
public I64 Read(I64 fd, U8 *buf, I64 n);
public I64 Write(I64 fd, U8 *buf, I64 n);
public I64 Close(I64 fd);
public I64 StdWrite(I64 fd, U8 *buf, I64 n);
public I64 Getpid();
public I64 Getppid();
public I64 Getuid();
public I64 Getgid();
public I64 Gettid();
public I64 Chdir(U8 *path);
public I64 Getcwd(U8 *buf, I64 size);
public I64 Mkdir(U8 *path, I64 mode);
public I64 WriteAll(I64 fd, U8 *buf, I64 n);

#endif
