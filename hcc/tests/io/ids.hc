
#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main() { "ppid=%d uid=%d gid=%d\n", Getppid() > 0, Getuid() >= 0, Getgid() >= 0; }
Main;
