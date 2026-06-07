#ifndef _NET_HC
#define _NET_HC
// net.hc — TCP networking over the raw BSD socket primitives.
//
// `Socket`/`Connect` are intrinsics: the prototypes live here, and the compiler
// lowers them to socket syscalls on the freestanding targets, or to libc on Darwin.
// The fd I/O primitives `Read`/`Write`/`Close` (and `WriteAll`) are shared with
// files, in `<io.hc>`. They do real, impure network I/O, so a program using them is
// not reproducible; conformance is by property, not by interp-vs-native value. On top
// of these primitives the module builds `ParseIPv4`, `MakeSockaddr`, `TcpConnect`, and
// a minimal `HttpGet`. Include with `#include <net.hc>`.

#include <io.hc>     // Read/Write/Close/WriteAll
#include <fmt.hc>    // StrPrint (for HttpGet)

#define AF_INET     2
#define SOCK_STREAM 1

// --- raw primitives (intrinsics) ---------------------------------------------

public I64 Socket(I64 domain, I64 kind, I64 proto);   // a socket fd, or -errno
public I64 Connect(I64 fd, U8 *addr, I64 len);        // 0, or -errno

// --- helpers ------------------------------------------------------------------

// Parse a dotted-quad "a.b.c.d" into a host-order U32.
public U32 ParseIPv4(U8 *s)
{
  U32 ip = 0;
  I64 octet = 0;
  while (*s) {
    if (*s == '.') { ip = (ip << 8) | (octet & 0xFF); octet = 0; }
    else { octet = octet * 10 + (*s - '0'); }
    s++;
  }
  return (ip << 8) | (octet & 0xFF);
}

// Fill a 16-byte `sockaddr_in` at `sa` for IPv4 `ip` (host order) and `port`:
// [sin_family (host U16 = 2)][sin_port (big-endian U16)][sin_addr (big-endian U32)][8×0].
public U0 MakeSockaddr(U8 *sa, U32 ip, I64 port)
{
  I64 i;
  for (i = 0; i < 16; i++) sa[i] = 0;
  sa[0] = AF_INET;            // little-endian host order for the family
  sa[2] = (port >> 8) & 0xFF; // network byte order
  sa[3] = port & 0xFF;
  sa[4] = (ip >> 24) & 0xFF;  // network byte order
  sa[5] = (ip >> 16) & 0xFF;
  sa[6] = (ip >> 8) & 0xFF;
  sa[7] = ip & 0xFF;
}

// Open a TCP connection to `ip` (host order) : `port`. Returns the fd, or -errno.
public I64 TcpConnect(U32 ip, I64 port)
{
  I64 fd = Socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) return fd;
  U8 sa[16];
  MakeSockaddr(sa, ip, port);
  I64 r = Connect(fd, sa, 16);
  if (r < 0) { Close(fd); return r; }
  return fd;
}

// HTTP/1.0 GET of `path` from the dotted-quad `ip_str` : `port`. Reads the whole
// raw response (status line + headers + body) into `buf` (capacity `cap`). Returns
// the byte count, or -errno. The caller NUL-terminates / parses it.
public I64 HttpGet(U8 *ip_str, I64 port, U8 *path, U8 *buf, I64 cap)
{
  I64 fd = TcpConnect(ParseIPv4(ip_str), port);
  if (fd < 0) return fd;
  U8 req[512];
  StrPrint(req, "GET %s HTTP/1.0\r\nHost: %s\r\n\r\n", path, ip_str);
  I64 w = WriteAll(fd, req, StrLen(req));
  if (w < 0) { Close(fd); return w; }
  I64 total = 0;
  while (total < cap) {
    I64 r = Read(fd, buf + total, cap - total);
    if (r <= 0) break;
    total += r;
  }
  Close(fd);
  return total;
}

#endif
