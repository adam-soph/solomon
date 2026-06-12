#ifndef _SOCKET_HC
#define _SOCKET_HC
// socket.hc — implementation (interface in socket.hh).

#include <socket.hh>
#include <unistd.hh>

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
