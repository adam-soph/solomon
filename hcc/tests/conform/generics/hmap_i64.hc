// hmap_i64.hc — Hmap<I64,I64>: put, get, has, del
#include <hmap.hh>
#include <stdio.hh>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 1, 100);
HmapPut(&m, 2, 200);
HmapPut(&m, 3, 300);
v, ok := HmapGet(&m, 2);
"2->%d ok=%d\n", v, ok;
_, miss := HmapGet(&m, 9);
"9 miss=%d\n", miss;
"has3=%d\n", HmapHas(&m, 3);
HmapDel(&m, 3);
"has3after=%d len=%d\n", HmapHas(&m, 3), HmapLen(&m);
HmapFree(&m);
