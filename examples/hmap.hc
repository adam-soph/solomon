// hmap.hc — the generic `<hmap.hc>` byte map, plus the thin typed facade you build on
// it and the tuple lookup it exists to show off: `(value, found)`, where `found` is
// what a sentinel can't express (a stored 0 looks identical to a miss otherwise).
//
// The map is generic over key/value *bytes* with key hashing/equality passed in, so a
// program wraps it once into the concrete type it wants. Here that's a string -> I64
// map (`Si*`): `HmapInit` with the stock `HmapStrHash`/`HmapStrEq`, then emplace-style
// `HmapPut`/`HmapGet` taking the key by address.

#include <hmap.hc>

// --- a string -> I64 facade over the generic byte map ---

U0 SiInit(Hmap *m)
{
  HmapInit(m, sizeof(U8 *), sizeof(I64), &HmapStrHash, &HmapStrEq, &HmapStrCopy);
}
U0 SiPut(Hmap *m, U8 *key, I64 v) { *(I64 *)HmapPut(m, &key) = v; }

(I64, Bool) SiGet(Hmap *m, U8 *key)
{
  (U8 *p, Bool found) = HmapGet(m, &key);
  if (found) return *(I64 *)p, TRUE;
  return 0, FALSE;
}

Bool SiHas(Hmap *m, U8 *key) { return HmapHas(m, &key); }
Bool SiDel(Hmap *m, U8 *key) { return HmapDel(m, &key); }

U0 Main()
{
  Hmap m;
  SiInit(&m);

  SiPut(&m, "zero", 0);      // a stored 0 — the case a sentinel can't distinguish
  SiPut(&m, "one", 1);
  SiPut(&m, "two", 2);
  SiPut(&m, "two", 22);      // update in place
  // Enough keys to force a rehash (load factor > 1 over 8 buckets).
  SiPut(&m, "a", 10);  SiPut(&m, "b", 11);  SiPut(&m, "c", 12);
  SiPut(&m, "d", 13);  SiPut(&m, "e", 14);  SiPut(&m, "f", 15);

  "len=%d\n", HmapLen(&m);

  // The tuple lookup: `found` separates "present, value 0" from "absent".
  (I64 z, Bool zf) = SiGet(&m, "zero");
  "zero: found=%d value=%d\n", zf, z;
  (I64 mv, Bool mf) = SiGet(&m, "missing");
  "missing: found=%d value=%d\n", mf, mv;

  "two=%d one=%d f=%d\n",
      SiGet(&m, "two")[0], SiGet(&m, "one")[0], SiGet(&m, "f")[0];

  "has(a)=%d del(a)=%d has(a)=%d\n", SiHas(&m, "a"), SiDel(&m, "a"), SiHas(&m, "a");
  "del(missing)=%d len=%d\n", SiDel(&m, "missing"), HmapLen(&m);

  // The map is unordered; `HmapSortKeys` dumps the surviving entries in sorted key
  // order (collect the keys into a Vec, sort, look each value back up).
  Vec keys;
  HmapSortKeys(&m, &keys, &HmapStrCmp);
  I64 i;
  for (i = 0; i < keys.len; i++) {
    U8 *kk = *(U8 **)VecAt(&keys, i);
    (I64 vv, Bool ok) = SiGet(&m, kk);
    "%s=%d ", kk, vv;
  }
  "\n";
  VecFree(&keys);

  // `HmapValues` — an order-independent aggregate over the values.
  Vec vals;
  HmapValues(&m, &vals);
  I64 sum = 0;
  for (i = 0; i < vals.len; i++) sum += *(I64 *)VecAt(&vals, i);
  "sum=%d count=%d\n", sum, vals.len;
  VecFree(&vals);

  // `HmapEntries` — each element is a `[key U8* | value I64]` block; sort by key (which
  // sits at offset 0) and read both straight from the block, no second lookup.
  Vec ents;
  HmapEntries(&m, &ents);
  VecSort(&ents, &HmapStrCmp);
  for (i = 0; i < ents.len; i++) {
    U8 *slot = VecAt(&ents, i);
    "%s:%d ", *(U8 **)slot, *(I64 *)(slot + sizeof(U8 *));
  }
  "\n";
  VecFree(&ents);

  HmapFree(&m);
}

Main;
