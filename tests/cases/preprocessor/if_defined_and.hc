// if_defined_and.hc — #if defined(A) && defined(B) pattern
#define FEATURE_X
#define FEATURE_Y

#if defined(FEATURE_X) && defined(FEATURE_Y)
"both on\n";
#endif

#if defined(FEATURE_X) && !defined(FEATURE_Z)
"X but not Z\n";
#endif
