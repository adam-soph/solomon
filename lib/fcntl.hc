#ifndef _FCNTL_HC
#define _FCNTL_HC
// fcntl.hc — C `<fcntl.h>`: `Open` and its flag constants.
//
// `Open` is an intrinsic: the prototype lives here, and the compiler lowers it to the
// host's `open`/`openat` syscall on the freestanding targets, or to libc on Darwin. It
// does real, impure I/O, so a program using it is not reproducible; conformance is by
// property, not by interp-vs-native value. Include with `#include <fcntl.hc>`.
//
// The flag values use the Linux `open(2)` numbers as the canonical set; the Darwin
// backend and the interpreter translate them to the host's flags, so the same constants
// work on every target. Errors are returned as a negative `-errno` (a non-negative
// result is the fd).

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR   2
#define O_CREAT  0x40
#define O_TRUNC  0x200
#define O_APPEND 0x400

// `rw-r--r--` create mode (octal, as conventional for Unix permission bits).
#define MODE_0644 0644

public I64 Open(U8 *path, I64 flags, I64 mode);   // a file fd, or -errno

#endif
