
// compare only first 3 chars
#include <stdio.hh>
#include <string.hh>
"%d\n", StrNCmp("abcXXX", "abcYYY", 3);
"%d\n", StrNCmp("abcX", "abdY", 3);
"%d\n", StrNCmp("abd", "abc", 3);
