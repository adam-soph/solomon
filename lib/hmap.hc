#ifndef _HMAP_HC
#define _HMAP_HC
// hmap.hc — implementation (interface in hmap.hh).

#include <hmap.hh>
#include <string.hh>

// ---- core (generic templates; prototypes in hmap.hh) ----

HmapEntry<K, V> **HmapNewBuckets<type K, type V>(I64 n)
{
  HmapEntry<K, V> **b = MAlloc(n * sizeof(HmapEntry<K, V> *));
  I64 i;
  for (i = 0; i < n; i++) b[i] = NULL;
  return b;
}

U0 HmapInit<type K, type V>(Hmap<K, V> *m, I64 (*hash)(K *), Bool (*eq)(K *, K *))
{
  m->nbuckets = HMAP_INIT_BUCKETS;
  m->buckets = HmapNewBuckets<K, V>(m->nbuckets);
  m->len = 0;
  m->hash = hash;
  m->eq = eq;
}

I64 HmapBucket<type K, type V>(Hmap<K, V> *m, K *key, I64 n)
{
  return (m->hash(key) & 0x7FFFFFFFFFFFFFFF) % n;
}

U0 HmapFree<type K, type V>(Hmap<K, V> *m)
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

U0 HmapRehash<type K, type V>(Hmap<K, V> *m)
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

U0 HmapPut<type K, type V>(Hmap<K, V> *m, K key, V val)
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

(V, Bool) HmapGet<type K, type V>(Hmap<K, V> *m, K key)
{
  HmapEntry<K, V> *e = m->buckets[HmapBucket<K, V>(m, &key, m->nbuckets)];
  while (e != NULL) {
    if (m->eq(&e->key, &key)) return e->val, TRUE;
    e = e->next;
  }
  V zero; // a declared-but-uninitialised local is zero-filled
  return zero, FALSE;
}

Bool HmapHas<type K, type V>(Hmap<K, V> *m, K key)
{
  _, ok := HmapGet<K, V>(m, key);
  return ok;
}

Bool HmapDel<type K, type V>(Hmap<K, V> *m, K key)
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

I64 HmapLen<type K, type V>(Hmap<K, V> *m) { return m->len; }

U0 HmapKeys<type K, type V>(Hmap<K, V> *m, Vec<K> *out)
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

U0 HmapValues<type K, type V>(Hmap<K, V> *m, Vec<V> *out)
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

U0 HmapSortKeys<type K, type V>(Hmap<K, V> *m, Vec<K> *out, I64 (*cmp)(K *, K *))
{
  HmapKeys<K, V>(m, out);
  VecSort<K>(out, cmp);
}

U0 HmapEntries<type K, type V>(Hmap<K, V> *m, Vec<HmapKV<K, V>> *out)
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

// ---- stock key hash/eq (non-generic) ----

// djb2 string hash, reduced to a non-negative I64 (a private helper for the stock
// string-key hashing below).
I64 Djb2(U8 *s)
{
  I64 h = 5381;
  I64 i = 0;
  while (s[i] != 0) { h = h * 33 + s[i]; i++; }
  return h & 0x7FFFFFFFFFFFFFFF;
}
public I64 HmapI64Hash(I64 *k) { I64 v = *k; return (v ^ (v >> 32)) & 0x7FFFFFFFFFFFFFFF; }
public Bool HmapI64Eq(I64 *a, I64 *b) { return *a == *b; }
public I64 HmapStrHash(U8 **k) { return Djb2(*k); }
public Bool HmapStrEq(U8 **a, U8 **b) { return StrCmp(*a, *b) == 0; }

#endif
