#include <string.hc>
U8 src[8]; U8 dst[8];
src[0] = 'a'; src[1] = 'b'; src[2] = 'c'; src[3] = 0; src[4] = 'd';
// copy until 'c' (inclusive) or 8 bytes
U8 *p = MemCCpy(dst, src, 'c', 8);
if (p) "%d\n", p - dst;
else "not found\n";
