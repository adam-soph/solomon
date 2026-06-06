#ifndef _STDIO_HC
#define _STDIO_HC
// stdio.hc — the standard streams. Just the portable stdout/stderr write primitive,
// split out from <io.hc> so the printf machinery (and any program that only prints)
// can depend on it without pulling in the file-descriptor helpers — which, with no
// dead-code elimination, would otherwise compile the whole <io.hc> file family into
// every printing program (and fail to build a file op the target doesn't support).
//
// `StdWrite(fd, buf, n)` writes to a standard stream — `fd` 1 = stdout, 2 = stderr —
// portably: the write syscall / libc on POSIX, `WriteFile(GetStdHandle(...))` on
// Windows. Returns bytes written, or -errno. (`<io.hc>`'s `Write` is the POSIX fd op;
// `StdWrite` is the one that also works on Windows.)

#define STDOUT 1
#define STDERR 2

I64 StdWrite(I64 fd, U8 *buf, I64 n);

#endif
