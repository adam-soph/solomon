// Connect to 127.0.0.1:__PORT__, send "ping", and print the echoed reply.
// `__PORT__` is substituted by the test harness with the live echo server's port.

#include <socket.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main() {
  I64 fd = Socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) { "connect failed: %d\n", -fd; return; }
  U8 sa[16];
  MakeSockaddr(sa, ParseIPv4("127.0.0.1"), __PORT__);
  if (Connect(fd, sa, 16) < 0) { "connect failed\n"; Close(fd); return; }
  Write(fd, "ping", 4);
  U8 buf[64];
  I64 n = Read(fd, buf, 64);
  if (n > 0) buf[n] = 0; else buf[0] = 0;
  "received: %s\n", buf;
  Close(fd);
}
Main;
