// if_relational.hc — #if with truthy/falsy integer macros and defined()
#define HAVE_FEATURE
#define ENABLE_LOG 1
#define DISABLE_X 0

#if ENABLE_LOG
"logging on\n";
#endif

#if DISABLE_X
"x enabled (unreached)\n";
#else
"x disabled\n";
#endif

#if defined(HAVE_FEATURE)
"feature present\n";
#endif

#if defined(HAVE_FEATURE) && ENABLE_LOG
"feature + log\n";
#endif
