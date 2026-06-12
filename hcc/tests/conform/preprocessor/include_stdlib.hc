// include_stdlib.hc — verify #include with stdlib works via macros

#include <stdio.hh>
#include <string.hh>
#define GREETING "hello"
I64 len = StrLen(GREETING);
"%s len=%d\n", GREETING, len;
