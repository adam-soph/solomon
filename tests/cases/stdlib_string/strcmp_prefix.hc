#include <string.hc>
// "abc" < "abcd" because after the shared prefix, 'abc' ends (NUL < 'd')
"%d\n", StrCmp("abc", "abcd");
"%d\n", StrCmp("abcd", "abc");
