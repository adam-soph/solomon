// ifdef_then_undef.hc — #ifdef, then #undef, then the guard is gone

#include <stdio.hh>
#define FLAG

#ifdef FLAG
"flag set\n";
#endif

#undef FLAG

#ifdef FLAG
"flag still set\n";
#else
"flag gone\n";
#endif
