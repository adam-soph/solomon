// defined_operator.hc — defined() operator in #if

#include <stdio.hh>
#define FOO
// BAR not defined

#if defined(FOO)
"FOO defined\n";
#endif

#if defined(BAR)
"BAR defined\n";
#else
"BAR not defined\n";
#endif

#if defined(FOO) && !defined(BAR)
"FOO and not BAR\n";
#endif
