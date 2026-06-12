// elif_three_way.hc — #elif three-way selection using defined flags

#include <stdio.hh>
#define LEVEL_MID
// LEVEL_LOW and LEVEL_HIGH not defined

#if defined(LEVEL_LOW)
"low\n";
#elif defined(LEVEL_MID)
"mid\n";
#elif defined(LEVEL_HIGH)
"high\n";
#else
"other\n";
#endif
