
#include <stdio.hh>
#include <stdlib.hh>
#include <unistd.hh>
U0 Main() {
  U8 *l;
  while ((l = ReadLine(STDIN))) { "<%s>\n", l; Free(l); }
}
Main;
