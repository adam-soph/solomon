// hmap.hc — the generic `<hmap.hc>` `Hmap<K, V>`, monomorphized here as a
// string -> I64 map, and the tuple lookup it exists to show off: `(value, found)`,
// where `found` is what a sentinel can't express (a stored 0 looks identical to a
// miss otherwise). Keys and values are typed — no casts, no element-size bookkeeping.

#include <hmap.hc>

U0 Main()
{
  Hmap<U8 *, I64> m;
  HmapInit(&m, &HmapStrHash, &HmapStrEq);   // stock string-key hash/eq

  HmapPut(&m, "zero", 0);    // a stored 0 — the case a sentinel can't distinguish
  HmapPut(&m, "one", 1);
  HmapPut(&m, "two", 2);
  HmapPut(&m, "two", 22);    // update in place
  // Enough keys to force a rehash (load factor > 1 over 8 buckets).
  HmapPut(&m, "a", 10);  HmapPut(&m, "b", 11);  HmapPut(&m, "c", 12);
  HmapPut(&m, "d", 13);  HmapPut(&m, "e", 14);  HmapPut(&m, "f", 15);

  "len=%d\n", HmapLen(&m);

  // The tuple lookup: `found` separates "present, value 0" from "absent".
  (I64 z, Bool zf) = HmapGet(&m, "zero");
  "zero: found=%d value=%d\n", zf, z;
  (I64 mv, Bool mf) = HmapGet(&m, "missing");
  "missing: found=%d value=%d\n", mf, mv;

  (I64 tv, Bool _t) = HmapGet(&m, "two");
  (I64 ov, Bool _o) = HmapGet(&m, "one");
  (I64 fv, Bool _f) = HmapGet(&m, "f");
  "two=%d one=%d f=%d\n", tv, ov, fv;

  "has(a)=%d del(a)=%d has(a)=%d\n", HmapHas(&m, "a"), HmapDel(&m, "a"), HmapHas(&m, "a");
  "del(missing)=%d len=%d\n", HmapDel(&m, "missing"), HmapLen(&m);

  // The map is unordered; `HmapSortKeys` dumps the surviving keys in sorted order
  // (collect into a Vec, sort by the stock string comparator, look each value back up).
  Vec<U8 *> keys;
  HmapSortKeys(&m, &keys, &CmpStr);
  I64 i;
  for (i = 0; i < VecLen(&keys); i++) {
    U8 *kk = VecAt(&keys, i);
    (I64 vv, Bool ok) = HmapGet(&m, kk);
    "%s=%d ", kk, vv;
  }
  "\n";
  VecFree(&keys);

  // `HmapValues` — an order-independent aggregate over the values.
  Vec<I64> vals;
  HmapValues(&m, &vals);
  I64 sum = 0;
  for (i = 0; i < VecLen(&vals); i++) sum += VecAt(&vals, i);
  "sum=%d count=%d\n", sum, VecLen(&vals);
  VecFree(&vals);

  // `HmapEntries` — each element is a `(key, val)` pair; sort by key (offset 0) and
  // read both straight from the pair, no second lookup.
  Vec<HmapKV<U8 *, I64>> ents;
  HmapEntries(&m, &ents);
  VecSort(&ents, &CmpStr);
  for (i = 0; i < VecLen(&ents); i++) {
    HmapKV<U8 *, I64> *kv = VecRef(&ents, i);
    "%s:%d ", kv->key, kv->val;
  }
  "\n";
  VecFree(&ents);

  HmapFree(&m);
}

Main;
