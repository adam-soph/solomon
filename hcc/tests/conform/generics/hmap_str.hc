// hmap_str.hc — Hmap<U8*,I64>: string keys
#include <hmap.hh>
#include <stdio.hh>
Hmap<U8 *, I64> m;
HmapInit(&m, &HmapStrHash, &HmapStrEq);
HmapPut(&m, "one", 1);
HmapPut(&m, "two", 2);
HmapPut(&m, "three", 3);
v1, _ := HmapGet(&m, "one");
v2, _ := HmapGet(&m, "two");
v3, _ := HmapGet(&m, "three");
"%d %d %d\n", v1, v2, v3;
HmapPut(&m, "two", 22);
v2b, _ := HmapGet(&m, "two");
"updated=%d\n", v2b;
HmapFree(&m);
