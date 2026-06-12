// hmap_sorted_keys.hc — HmapSortKeys collects and sorts keys
#include <hmap.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <vec.hh>
Hmap<I64, I64> m;
HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
HmapPut(&m, 30, 3);
HmapPut(&m, 10, 1);
HmapPut(&m, 20, 2);
Vec<I64> keys;
HmapSortKeys(&m, &keys, &CmpI64);
I64 i;
for (i = 0; i < VecLen(&keys); i++)
  "%d ", VecAt(&keys, i);
"\n";
VecFree(&keys);
HmapFree(&m);
