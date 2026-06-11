#include <string.hc>
U8 *a = "apple";
U8 *b = "banana";
U8 *c = "apple";
"%d\n", CmpStr(&a, &b);
"%d\n", CmpStr(&a, &c);
"%d\n", CmpStr(&b, &a);
