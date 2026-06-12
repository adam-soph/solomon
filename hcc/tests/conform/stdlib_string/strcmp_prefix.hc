
// "abc" < "abcd" because after the shared prefix, 'abc' ends (NUL < 'd')
#include <stdio.hh>
#include <string.hh>
"%d\n", StrCmp("abc", "abcd");
"%d\n", StrCmp("abcd", "abc");
