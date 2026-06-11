// tuple_generic_call.hc — unpack a generic call's tuple return with :=
#include <hmap.hc>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 7, 49);
HmapPut(&m, 3, 9);
val, ok := HmapGet(&m, 7);
"7->%d ok=%d\n", val, ok;
miss, found := HmapGet(&m, 99);
"99 found=%d\n", found;
HmapFree(&m);
