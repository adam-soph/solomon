#ifndef _HMAP_HC
#define _HMAP_HC
// hmap.hc — `Hmap<K, V>`, an owning generic hash map (separate chaining over a growing
// bucket array), monomorphized per (key, value) type. Keys and values are typed — no
// casts. The key's hashing and equality are function pointers given at `HmapInit` (HolyC
// can't derive them), each taking a `K *`:
//
//     Hmap<U8 *, I64> m;
//     HmapInit(&m, &HmapStrHash, &HmapStrEq);   // string keys
//     HmapPut(&m, "answer", 42);
//     v, ok := HmapGet(&m, "answer");           // `ok` distinguishes a stored 0 from a miss
//
// Stock key ops: `HmapI64Hash`/`HmapI64Eq` (I64 keys) and `HmapStrHash`/`HmapStrEq`
// (`U8 *` string keys — content-hashed/compared). Built on <mem.hc>, <cstr.hc> (string
// keys), and <vec.hc> (`HmapKeys`/`HmapValues`). Pure HolyC, identical on the interpreter
// and every backend. Include with `#include <hmap.hc>`.
//
// The caller owns the `Hmap`; `HmapInit` is required before use. `Hmap` owns its entries
// (free with `HmapFree`); a `U8 *` string key stores the pointer, so it must outlive the
// map (string *literals* do).

#include <cstr.hc>
#include <mem.hc>
#include <vec.hc>
#include <_impl/strhash.hc>   // private djb2 (the `_impl/` privacy demo)

#define HMAP_INIT_BUCKETS 8

class HmapEntry<K, V> {
  HmapEntry<K, V> *next; // a chain of entries in the same bucket
  K key;
  V val;
}

class Hmap<K, V> {
  HmapEntry<K, V> **buckets; // heap array of `nbuckets` chain heads
  I64 nbuckets;
  I64 len;
  I64 (*hash)(K *key);
  Bool (*eq)(K *a, K *b);
}

// ---- stock key hash/eq (passed to HmapInit) ----

I64 HmapI64Hash(I64 *k) { I64 v = *k; return (v ^ (v >> 32)) & 0x7FFFFFFFFFFFFFFF; }
Bool HmapI64Eq(I64 *a, I64 *b) { return *a == *b; }
// String keys: the key is a `U8 *`, so the op takes `U8 **` and dereferences it. `Djb2`
// is the private `<_impl/strhash.hc>` helper, reachable from the stdlib subtree.
I64 HmapStrHash(U8 **k) { return Djb2(*k); }
Bool HmapStrEq(U8 **a, U8 **b) { return StrCmp(*a, *b) == 0; }

// ---- core ----

HmapEntry<K, V> **HmapNewBuckets<K, V>(I64 n)
{
  HmapEntry<K, V> **b = MAlloc(n * sizeof(HmapEntry<K, V> *));
  I64 i;
  for (i = 0; i < n; i++) b[i] = NULL;
  return b;
}

U0 HmapInit<K, V>(Hmap<K, V> *m, I64 (*hash)(K *), Bool (*eq)(K *, K *))
{
  m->nbuckets = HMAP_INIT_BUCKETS;
  m->buckets = HmapNewBuckets<K, V>(m->nbuckets);
  m->len = 0;
  m->hash = hash;
  m->eq = eq;
}

// Bucket index for a key (the sign mask keeps a user hash returning a negative I64 in
// range).
I64 HmapBucket<K, V>(Hmap<K, V> *m, K *key, I64 n)
{
  return (m->hash(key) & 0x7FFFFFFFFFFFFFFF) % n;
}

U0 HmapFree<K, V>(Hmap<K, V> *m)
{
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry<K, V> *e = m->buckets[i];
    while (e != NULL) {
      HmapEntry<K, V> *next = e->next;
      Free(e);
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = NULL;
  m->nbuckets = 0;
  m->len = 0;
}

U0 HmapRehash<K, V>(Hmap<K, V> *m)
{
  I64 newn = m->nbuckets * 2;
  HmapEntry<K, V> **nb = HmapNewBuckets<K, V>(newn);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry<K, V> *e = m->buckets[i];
    while (e != NULL) {
      HmapEntry<K, V> *next = e->next;
      I64 b = HmapBucket<K, V>(m, &e->key, newn);
      e->next = nb[b];
      nb[b] = e;
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = nb;
  m->nbuckets = newn;
}

// Insert `key -> val`, or update the value if `key` is already present.
U0 HmapPut<K, V>(Hmap<K, V> *m, K key, V val)
{
  I64 b = HmapBucket<K, V>(m, &key, m->nbuckets);
  HmapEntry<K, V> *e = m->buckets[b];
  while (e != NULL) {
    if (m->eq(&e->key, &key)) {
      e->val = val;
      return;
    }
    e = e->next;
  }
  HmapEntry<K, V> *node = MAlloc(sizeof(HmapEntry<K, V>));
  node->key = key;
  node->val = val;
  node->next = m->buckets[b];
  m->buckets[b] = node;
  m->len++;
  if (m->len > m->nbuckets) HmapRehash<K, V>(m);
}

// Look up `key`. Returns `(value, TRUE)` when present, else `(zero, FALSE)` — the flag
// distinguishes a stored value from a miss.
(V, Bool) HmapGet<K, V>(Hmap<K, V> *m, K key)
{
  HmapEntry<K, V> *e = m->buckets[HmapBucket<K, V>(m, &key, m->nbuckets)];
  while (e != NULL) {
    if (m->eq(&e->key, &key)) return e->val, TRUE;
    e = e->next;
  }
  V zero; // a declared-but-uninitialised local is zero-filled
  return zero, FALSE;
}

Bool HmapHas<K, V>(Hmap<K, V> *m, K key)
{
  _, ok := HmapGet<K, V>(m, key);
  return ok;
}

// Remove `key`, freeing its entry. Returns TRUE if it was present.
Bool HmapDel<K, V>(Hmap<K, V> *m, K key)
{
  I64 b = HmapBucket<K, V>(m, &key, m->nbuckets);
  HmapEntry<K, V> *e = m->buckets[b];
  HmapEntry<K, V> *prev = NULL;
  while (e != NULL) {
    if (m->eq(&e->key, &key)) {
      if (prev == NULL) m->buckets[b] = e->next;
      else prev->next = e->next;
      Free(e);
      m->len--;
      return TRUE;
    }
    prev = e;
    e = e->next;
  }
  return FALSE;
}

I64 HmapLen<K, V>(Hmap<K, V> *m) { return m->len; }

// Collect all keys / values into `out` (a `Vec<K>` / `Vec<V>`, initialised here). Order
// is unspecified (bucket order). `HmapSortKeys` sorts the keys by `cmp`. Free `out` with
// `VecFree`.
U0 HmapKeys<K, V>(Hmap<K, V> *m, Vec<K> *out)
{
  VecInit<K>(out);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry<K, V> *e = m->buckets[i];
    while (e != NULL) {
      VecPush<K>(out, e->key);
      e = e->next;
    }
  }
}

U0 HmapValues<K, V>(Hmap<K, V> *m, Vec<V> *out)
{
  VecInit<V>(out);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry<K, V> *e = m->buckets[i];
    while (e != NULL) {
      VecPush<V>(out, e->val);
      e = e->next;
    }
  }
}

// Collect the keys (as `HmapKeys`) and sort them by `cmp` — a comparator over *key
// element pointers* (`K *`), e.g. `&CmpStr` for `U8 *` keys or `&CmpI64` for `I64`.
U0 HmapSortKeys<K, V>(Hmap<K, V> *m, Vec<K> *out, I64 (*cmp)(U8 *, U8 *))
{
  HmapKeys<K, V>(m, out);
  VecSort<K>(out, cmp);
}

// A key/value pair, the element type `HmapEntries` collects (a copy, detached from the
// map's internal chain — no `next` link). `key` sits at offset 0, so a `Vec<HmapKV>`
// can be `VecSort`ed by a key comparator (`&CmpStr` / `&CmpI64`).
class HmapKV<K, V> {
  K key;
  V val;
}

// Collect every entry as a `(key, val)` pair into `out` (a `Vec<HmapKV<K, V>>`,
// initialised here), in unspecified (bucket) order. Free `out` with `VecFree`.
U0 HmapEntries<K, V>(Hmap<K, V> *m, Vec<HmapKV<K, V>> *out)
{
  VecInit<HmapKV<K, V>>(out);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry<K, V> *e = m->buckets[i];
    while (e != NULL) {
      HmapKV<K, V> kv;
      kv.key = e->key;
      kv.val = e->val;
      VecPush<HmapKV<K, V>>(out, kv);
      e = e->next;
    }
  }
}

#endif
