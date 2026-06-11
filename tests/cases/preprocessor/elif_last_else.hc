// elif_last_else.hc — all elif branches false, else taken
// Only NONE_MATCH is defined (none of the specific ones)
#define NONE_MATCH
// X_ONE, X_TWO, X_THREE not defined

#if defined(X_ONE)
"one\n";
#elif defined(X_TWO)
"two\n";
#elif defined(X_THREE)
"three\n";
#else
"other\n";
#endif
