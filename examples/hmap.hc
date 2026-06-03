// hmap.hc — the `<hmap.hc>` string->I64 hash map, and the tuple lookup it exists to
// show off: `(I64 value, Bool found)`, where `found` is what a sentinel can't express
// (a stored 0 looks identical to a miss otherwise).

#include <hmap.hc>

U0 Main()
{
  Hmap m;
  HmapInit(&m);

  HmapPut(&m, "zero", 0);      // a stored 0 — the case a sentinel can't distinguish
  HmapPut(&m, "one", 1);
  HmapPut(&m, "two", 2);
  HmapPut(&m, "two", 22);      // update in place
  // Enough keys to force a rehash (load factor > 1 over 8 buckets).
  HmapPut(&m, "a", 10);  HmapPut(&m, "b", 11);  HmapPut(&m, "c", 12);
  HmapPut(&m, "d", 13);  HmapPut(&m, "e", 14);  HmapPut(&m, "f", 15);

  "len=%d\n", HmapLen(&m);

  // The tuple lookup: `found` separates "present, value 0" from "absent".
  (I64 z, Bool zf) = HmapGet(&m, "zero");
  "zero: found=%d value=%d\n", zf, z;
  (I64 mv, Bool mf) = HmapGet(&m, "missing");
  "missing: found=%d value=%d\n", mf, mv;

  "two=%d one=%d f=%d\n",
      HmapGet(&m, "two")[0], HmapGet(&m, "one")[0], HmapGet(&m, "f")[0];

  "has(a)=%d del(a)=%d has(a)=%d\n", HmapHas(&m, "a"), HmapDel(&m, "a"), HmapHas(&m, "a");
  "del(missing)=%d len=%d\n", HmapDel(&m, "missing"), HmapLen(&m);

  HmapFree(&m);
}

Main;
