// hmap_values.hc — HmapValues collects all values
#include <hmap.hc>
#include <stdlib.hc>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 10, 100);
HmapPut(&m, 20, 200);
HmapPut(&m, 30, 300);
Vec<I64> vals;
HmapValues(&m, &vals);
VecSort(&vals, &CmpI64);
I64 i;
for (i = 0; i < VecLen(&vals); i++) "%d ", VecAt(&vals, i);
"\n";
VecFree(&vals);
HmapFree(&m);
