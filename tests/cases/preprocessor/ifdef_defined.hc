// ifdef_defined.hc — #ifdef with defined and undefined macros
#define FEATURE_A
// FEATURE_B not defined

#ifdef FEATURE_A
"A is on\n";
#endif

#ifdef FEATURE_B
"B is on\n";
#else
"B is off\n";
#endif
