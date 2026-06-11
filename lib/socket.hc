#ifndef _SOCKET_HC
#define _SOCKET_HC
// socket.hc — TCP networking over the raw BSD socket primitives (C `<sys/socket.h>` /
// `<netinet/in.h>` / `<arpa/inet.h>`, consolidated).
//
// `Socket`/`Connect` are intrinsics: the prototypes live here, and the compiler lowers
// them to socket syscalls on the freestanding targets, or to libc on Darwin. The fd I/O
// primitives `Read`/`Write`/`Close` (and `WriteAll`) are shared with files, in
// `<unistd.hc>`. They do real, impure network I/O, so a program using them is not
// reproducible; conformance is by property, not by interp-vs-native value. On top of
// these primitives the module builds the `ParseIPv4` and `MakeSockaddr` address helpers;
// connecting is `Socket` + `MakeSockaddr` + `Connect`. Include with `#include <socket.hc>`.

#include <unistd.hc>   // Read/Write/Close/WriteAll

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

#endif
