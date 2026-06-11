#include <string.hc>
U8 hay[4]; hay[0] = 'a'; hay[1] = 'b'; hay[2] = 'c';
// empty needle matches at start
U8 *p = MemSearch(hay, 3, hay, 0);
"%d\n", p == hay;
