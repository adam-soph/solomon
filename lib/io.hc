#ifndef _IO_HC
#define _IO_HC
// io.hc — file descriptor I/O: the raw `Read`/`Write`/`Close`/`Open`/`LSeek`
// primitives plus file-level helpers (`ReadFile`/`WriteFile`/`AppendFile`/`FileSize`).
//
// `Read`/`Write`/`Close`/`Open`/`LSeek` are **intrinsics** (prototypes here; the
// compiler lowers them to the host's file syscalls on the freestanding targets, libc
// on Darwin). They do real, impure I/O, so a program using them is *not* reproducible
// — conformance is by property, not by interp-vs-native value. `lib/net.hc` shares the
// same `Read`/`Write`/`Close` for sockets. Include with `#include <io.hc>`.
//
// **Errors are the return value**, Unix-style: a successful call returns a
// non-negative result (an fd, a byte count, an offset); a failure returns a negative
// `-errno`. There is no out-parameter and no error object — you test the sign:
//
//     I64 n = ReadFile("/cfg", buf, sizeof(buf));
//     if (n < 0) { "read failed (errno %d)\n", -n; return; }

#include <cstr.hc>   // StrLen

// `Open` flags use the **Linux** `open(2)` values as the canonical set; the Darwin
// backend and the interpreter translate them to the host's flags, so the same
// constants work on every target.
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR   2
#define O_CREAT  0x40
#define O_TRUNC  0x200
#define O_APPEND 0x400

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

// `rw-r--r--` create mode (octal, as conventional for Unix permission bits).
#define MODE_0644 0644

// --- raw primitives (intrinsics) ---------------------------------------------

I64 Open(U8 *path, I64 flags, I64 mode);   // a file fd, or -errno
I64 LSeek(I64 fd, I64 off, I64 whence);    // new absolute offset, or -errno
I64 Read(I64 fd, U8 *buf, I64 n);          // bytes read (0 = EOF), or -errno
I64 Write(I64 fd, U8 *buf, I64 n);         // bytes written, or -errno
I64 Close(I64 fd);                         // 0, or -errno

// (Filesystem mutation — `Remove`/`Rename`/`Mkdir` — and process control live in
// `<os.hc>`.)

// --- fd helpers ---------------------------------------------------------------

// Write all `n` bytes to `fd`, looping over partial writes. Returns 0, or -errno.
I64 WriteAll(I64 fd, U8 *buf, I64 n)
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

// --- file helpers -------------------------------------------------------------

// Size of `path` in bytes, or -errno.
I64 FileSize(U8 *path)
{
  I64 fd = Open(path, O_RDONLY, 0);
  if (fd < 0) return fd;
  I64 n = LSeek(fd, 0, SEEK_END);
  Close(fd);
  return n;  // LSeek already yields -errno on failure
}

// Read up to `cap` bytes of `path` into `buf`. Returns the byte count, or -errno. The
// caller NUL-terminates / parses the result.
I64 ReadFile(U8 *path, U8 *buf, I64 cap)
{
  I64 fd = Open(path, O_RDONLY, 0);
  if (fd < 0) return fd;
  I64 total = 0;
  while (total < cap) {
    I64 r = Read(fd, buf + total, cap - total);
    if (r < 0) { Close(fd); return r; }
    if (r == 0) break;  // EOF
    total += r;
  }
  Close(fd);
  return total;
}

// Create/truncate `path` (mode 0644) and write `n` bytes. Returns 0, or -errno.
I64 WriteFile(U8 *path, U8 *buf, I64 n)
{
  I64 fd = Open(path, O_WRONLY | O_CREAT | O_TRUNC, MODE_0644);
  if (fd < 0) return fd;
  I64 r = WriteAll(fd, buf, n);
  Close(fd);
  return r;
}

// Append `n` bytes to `path` (creating it, mode 0644). Returns 0, or -errno.
I64 AppendFile(U8 *path, U8 *buf, I64 n)
{
  I64 fd = Open(path, O_WRONLY | O_CREAT | O_APPEND, MODE_0644);
  if (fd < 0) return fd;
  I64 r = WriteAll(fd, buf, n);
  Close(fd);
  return r;
}

#endif
