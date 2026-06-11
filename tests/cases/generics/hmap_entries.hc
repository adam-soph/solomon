// hmap_entries.hc — HmapEntries collects key/value pairs
#include <hmap.hc>
#include <stdlib.hc>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 1, 10);
HmapPut(&m, 2, 20);
HmapPut(&m, 3, 30);
Vec<HmapKV<I64, I64>> entries;
HmapEntries(&m, &entries);
// Sort by key for deterministic output
VecSort(&entries, &CmpI64);
I64 i;
for (i = 0; i < VecLen(&entries); i++) {
  HmapKV<I64, I64> kv = VecAt(&entries, i);
  "%d->%d\n", kv.key, kv.val;
}
VecFree(&entries);
HmapFree(&m);
