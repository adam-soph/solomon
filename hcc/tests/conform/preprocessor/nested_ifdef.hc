// nested_ifdef.hc — nested #ifdef blocks

#include <stdio.hh>
#define OUTER
#define INNER

#ifdef OUTER
  "outer on\n";
  #ifdef INNER
    "inner on\n";
  #else
    "inner off\n";
  #endif
#else
  "outer off\n";
#endif
