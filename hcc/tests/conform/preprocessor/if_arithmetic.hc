// if_arithmetic.hc — #if with truthy integer macros and defined checks

#include <stdio.hh>
#define BIG_AREA
#define CORRECT_PERIMETER

#if defined(BIG_AREA)
"big enough\n";
#else
"too small\n";
#endif

#if defined(CORRECT_PERIMETER)
"perimeter half = 7\n";
#endif

// Use integer macros as booleans
#define HAVE_X 1
#define HAVE_Y 0
#if HAVE_X
"have x\n";
#endif
#if HAVE_Y
"have y (unreached)\n";
#else
"no y\n";
#endif
