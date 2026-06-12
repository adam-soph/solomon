// macro_string_const.hc — string constant macros

#include <stdio.hh>
#define PREFIX "Hello"
#define SUFFIX "World"
#define SEP ", "
// Print each separately (no string concat in HolyC macros)
"%s%s%s\n", PREFIX, SEP, SUFFIX;
