#include <string.hc>
// equal up to n
"%d\n", StrNCmp("hello", "hello", 5);
"%d\n", StrNCmp("hello", "hello", 0);
// differ at position n: should still return 0 if n is before that
"%d\n", StrNCmp("helloX", "helloY", 5);
