#include <string.hc>
U8 *p = StrNDup("hello world", 5);
"%s\n", p;
Free(p);
