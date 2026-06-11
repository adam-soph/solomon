// if_integer_expr.hc — #if with integer constant truthy/falsy values
#define VERSION_2
#define VERSION_3
// These are flags not values; use defined() for integer-based logic
#define FLAG_ON 1
#define FLAG_OFF 0

#if FLAG_ON
"flag on\n";
#endif

#if FLAG_OFF
"flag off (unreached)\n";
#else
"flag off branch\n";
#endif

#if defined(VERSION_3)
"version 3\n";
#elif defined(VERSION_2)
"version 2\n";
#else
"other\n";
#endif
