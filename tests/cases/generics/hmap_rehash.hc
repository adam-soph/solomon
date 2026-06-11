// hmap_rehash.hc — insert enough entries to trigger a rehash
#include <hmap.hc>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
I64 i;
for (i = 0; i < 20; i++) HmapPut(&m, i, i * i);
"len=%d\n", HmapLen(&m);
v, ok := HmapGet(&m, 15);
"15->%d ok=%d\n", v, ok;
v2, ok2 := HmapGet(&m, 0);
"0->%d ok=%d\n", v2, ok2;
HmapFree(&m);
