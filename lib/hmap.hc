#ifndef _HMAP_HC
#define _HMAP_HC
// hmap.hc — `Hmap`, an owning hash map from NUL-terminated string keys to `I64`
// values (a `map[string]int`). Separate chaining over a heap bucket array that
// grows (rehashes) as it fills. Built on <cstr.hc> (string ops) and <mem.hc> (heap).
// Pure HolyC, identical on the interpreter and every backend. Include with
// `#include <hmap.hc>`.
//
// The lookup returns a **tuple** `(I64 value, Bool found)` — the `found` flag is what
// a sentinel can't express, since any `I64` (including 0 or -1) is a valid stored
// value:
//
//     Hmap m; HmapInit(&m);
//     HmapPut(&m, "answer", 42);
//     (I64 v, Bool ok) = HmapGet(&m, "answer");
//     if (ok) "answer = %d\n", v;
//
// The caller owns the `Hmap` struct; methods take `Hmap *`. `HmapInit(&m)` is
// **required** before use. `Hmap` owns its entries and key copies — free with
// `HmapFree`.

#include <cstr.hc>
#include <mem.hc>

#define HMAP_INIT_BUCKETS 8

class HmapEntry {
  U8 *key;            // owned heap copy of the key
  I64 value;
  HmapEntry *next;    // next entry in the same bucket's chain
}

class Hmap {
  HmapEntry **buckets;  // heap array of `nbuckets` chain heads
  I64 nbuckets;
  I64 len;              // number of key/value pairs
}

// djb2 hash of a NUL-terminated string (kept non-negative for the `% nbuckets`).
I64 HmapHash(U8 *s)
{
  I64 h = 5381;
  I64 i = 0;
  while (s[i] != 0) { h = h * 33 + s[i]; i++; }
  return h & 0x7FFFFFFFFFFFFFFF;
}

// A heap array of `n` chain heads, all NULL.
HmapEntry **HmapNewBuckets(I64 n)
{
  HmapEntry **b = MAlloc(n * sizeof(HmapEntry *));
  I64 i;
  for (i = 0; i < n; i++) b[i] = NULL;
  return b;
}

// Initialise an empty map. Required before any other call.
U0 HmapInit(Hmap *m)
{
  m->nbuckets = HMAP_INIT_BUCKETS;
  m->buckets = HmapNewBuckets(m->nbuckets);
  m->len = 0;
}

// Free every entry (and its key copy) and the bucket array; return to the empty
// state. The `Hmap` struct itself is the caller's.
U0 HmapFree(Hmap *m)
{
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry *e = m->buckets[i];
    while (e != NULL) {
      HmapEntry *next = e->next;
      Free(e->key);
      Free(e);
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = NULL;
  m->nbuckets = 0;
  m->len = 0;
}

// Double the bucket count and re-link every entry into the new array. The entries
// (and their keys) are reused — only the chain heads move.
U0 HmapRehash(Hmap *m)
{
  I64 newn = m->nbuckets * 2;
  HmapEntry **nb = HmapNewBuckets(newn);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    HmapEntry *e = m->buckets[i];
    while (e != NULL) {
      HmapEntry *next = e->next;
      I64 b = HmapHash(e->key) % newn;
      e->next = nb[b];
      nb[b] = e;
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = nb;
  m->nbuckets = newn;
}

// Insert `key`->`value`, or update the value if `key` is already present. The key is
// copied onto the heap, so the caller's buffer need not outlive the map.
U0 HmapPut(Hmap *m, U8 *key, I64 value)
{
  I64 b = HmapHash(key) % m->nbuckets;
  HmapEntry *e = m->buckets[b];
  while (e != NULL) {
    if (StrCmp(e->key, key) == 0) { e->value = value; return; }
    e = e->next;
  }
  HmapEntry *node = MAlloc(sizeof(HmapEntry));
  node->key = MAlloc(StrLen(key) + 1);
  StrCpy(node->key, key);
  node->value = value;
  node->next = m->buckets[b];
  m->buckets[b] = node;
  m->len++;
  if (m->len > m->nbuckets) HmapRehash(m);  // keep the load factor near 1
}

// Look up `key`. Returns `(value, TRUE)` when present, else `(0, FALSE)` — the flag
// is what distinguishes a stored 0 from a miss.
(I64, Bool) HmapGet(Hmap *m, U8 *key)
{
  HmapEntry *e = m->buckets[HmapHash(key) % m->nbuckets];
  while (e != NULL) {
    if (StrCmp(e->key, key) == 0) return e->value, TRUE;
    e = e->next;
  }
  return 0, FALSE;
}

// Is `key` present?
Bool HmapHas(Hmap *m, U8 *key)
{
  (I64 _, Bool ok) = HmapGet(m, key);
  return ok;
}

// Remove `key`, freeing its entry. Returns TRUE if it was present.
Bool HmapDel(Hmap *m, U8 *key)
{
  I64 b = HmapHash(key) % m->nbuckets;
  HmapEntry *e = m->buckets[b];
  HmapEntry *prev = NULL;
  while (e != NULL) {
    if (StrCmp(e->key, key) == 0) {
      if (prev == NULL) m->buckets[b] = e->next;
      else prev->next = e->next;
      Free(e->key);
      Free(e);
      m->len--;
      return TRUE;
    }
    prev = e;
    e = e->next;
  }
  return FALSE;
}

// The number of key/value pairs.
I64 HmapLen(Hmap *m) { return m->len; }

#endif
