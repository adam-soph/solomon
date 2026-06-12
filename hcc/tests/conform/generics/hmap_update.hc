// hmap_update.hc — HmapPut overwrites existing key
#include <hmap.hh>
#include <stdio.hh>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 5, 50);
v1, _ := HmapGet(&m, 5);
"before=%d\n", v1;
HmapPut(&m, 5, 555);
v2, _ := HmapGet(&m, 5);
"after=%d len=%d\n", v2, HmapLen(&m);
HmapFree(&m);
