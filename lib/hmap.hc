#ifndef _HMAP_HC
#define _HMAP_HC
// hmap.hc — `Hmap`, an owning, generic hash map: keys of `ksize` bytes to values of
// `vsize` bytes, with the key's hashing and equality supplied as function pointers at
// `HmapInit`. Separate chaining over a heap bucket array that grows (rehashes) as it
// fills. Built on <mem.hc> (heap) and <cstr.hc> (the stock string key ops). Pure
// HolyC, identical on the interpreter and every backend. Include with
// `#include <hmap.hc>`.
//
// Generic like <vec.hc>: one `Hmap` type holds *any* fixed-size key and value, chosen
// at `HmapInit(&m, ksize, vsize, hash, eq, copy)`. The key's behaviour is supplied as
// three function pointers — `hash` and `eq` over key slots, and `copy` to move a key
// into an entry (a typed move, not a raw `MemCpy`, so a pointer key keeps its identity).
// Values are stored by value in a byte buffer; access is by emplace pointer — `HmapPut`
// returns a `U8 *` value slot to write through, `HmapGet` returns `(U8 *value, Bool found)`:
//
//     Hmap m;
//     HmapInit(&m, sizeof(I64), sizeof(I64), &HmapI64Hash, &HmapI64Eq, &HmapI64Copy);
//     I64 k = 7;
//     *(I64 *)HmapPut(&m, &k) = 42;            // emplace: write into the value slot
//     (U8 *vp, Bool ok) = HmapGet(&m, &k);
//     if (ok) "got %d\n", *(I64 *)vp;
//
// `HmapPut`/`HmapGet`/`HmapHas`/`HmapDel` take the key **by address** (`&k`) — a pointer
// to the `ksize` key bytes. `hash`/`eq`/`copy` read that pointer at offset 0, so the
// key may be a stack local. The `found` flag is what a sentinel can't express (any
// stored value, incl. 0/-1, is valid).
//
// Two stock key kinds are provided so callers rarely hand-roll the trio:
//   * `HmapI64Hash`/`HmapI64Eq`/`HmapI64Copy` — an `I64` (or any 8-byte POD) key,
//     compared by value.
//   * `HmapStrHash`/`HmapStrEq`/`HmapStrCopy` — a `U8 *` string key: the key slot holds
//     the pointer and the ops dereference it (djb2 hash, `StrCmp`). The map stores the
//     **pointer**, not a copy, so a string key must outlive the map (string *literals*
//     do, having a stable address). To map strings, use `ksize = sizeof(U8 *)`.
//
// A thin typed facade (see examples/hmap.hc) wraps these into one-liners like
// `SiPut(&m, "k", v)`. The caller owns the `Hmap` struct; methods take `Hmap *`.
// `HmapInit` is **required** before use. `Hmap` owns its entries (free with `HmapFree`)
// but **not** the bytes a key/value pointer may point at.

#include <cstr.hc>
#include <mem.hc>
#include <vec.hc>             // HmapKeys/HmapSortKeys collect into a Vec (pulls in <sort.hc>)
#include <_impl/strhash.hc>   // private djb2 helper (see the `_impl/` privacy demo)

#define HMAP_INIT_BUCKETS 8

// An `Hmap` owns a heap array of chain heads. Each entry is a raw byte block laid out
// `[ next pointer (8) | key (ksize) | value (vsize) ]` — chained through the leading
// pointer, which is byte-serialised like any pointer stored in a heap buffer, so the
// generic inline key/value storage behaves the same on the interpreter and natively.
class Hmap {
  U8 **buckets;             // heap array of `nbuckets` chain heads (each an entry base, or NULL)
  I64 nbuckets;
  I64 len;                  // number of key/value pairs
  I64 ksize;                // key size in bytes
  I64 vsize;                // value size in bytes
  I64 (*hash)(U8 *key);          // hash a key slot to a (sign-masked) bucket-able I64
  Bool (*eq)(U8 *a, U8 *b);      // are two key slots equal?
  U0 (*copy)(U8 *dst, U8 *src);  // move a key into an entry slot (a typed move)
}

// ---- entry layout helpers (an entry is a `U8 *` byte block) ----

// The next-entry pointer lives in the first 8 bytes.
U8 *HmapNext(U8 *e)            { return *(U8 **)e; }
U0 HmapSetNext(U8 *e, U8 *n)  { *(U8 **)e = n; }
// The key bytes follow the next pointer; the value bytes follow the key.
U8 *HmapKeyOf(U8 *e)             { return e + sizeof(U8 *); }
U8 *HmapValOf(Hmap *m, U8 *e)    { return e + sizeof(U8 *) + m->ksize; }

// ---- stock key hash/eq ----

// An I64 (or any 8-byte POD) key, hashed/compared/copied by value. `HmapI64Cmp` is the
// <0/0/>0 ordering for `HmapSortKeys`.
I64 HmapI64Hash(U8 *k)            { I64 v = *(I64 *)k; return (v ^ (v >> 32)) & 0x7FFFFFFFFFFFFFFF; }
Bool HmapI64Eq(U8 *a, U8 *b)      { return *(I64 *)a == *(I64 *)b; }
U0 HmapI64Copy(U8 *dst, U8 *src)  { *(I64 *)dst = *(I64 *)src; }
I64 HmapI64Cmp(U8 *a, U8 *b)      { I64 x = *(I64 *)a, y = *(I64 *)b; return x < y ? -1 : x > y; }

// A `U8 *` string key: the slot holds the pointer; dereference it (and copy it as a
// pointer, preserving identity). `HmapStrHash` uses the private `<_impl/strhash.hc>`
// djb2 — reachable here because hmap.hc is in the standard-library subtree that
// `_impl/` is private to. `HmapStrCmp` is the lexicographic ordering for `HmapSortKeys`.
I64 HmapStrHash(U8 *k)            { return Djb2(*(U8 **)k); }
Bool HmapStrEq(U8 *a, U8 *b)      { return StrCmp(*(U8 **)a, *(U8 **)b) == 0; }
U0 HmapStrCopy(U8 *dst, U8 *src)  { *(U8 **)dst = *(U8 **)src; }
I64 HmapStrCmp(U8 *a, U8 *b)      { return StrCmp(*(U8 **)a, *(U8 **)b); }

// ---- core ----

// A heap array of `n` chain heads, all NULL.
U8 **HmapNewBuckets(I64 n)
{
  U8 **b = MAlloc(n * sizeof(U8 *));
  I64 i;
  for (i = 0; i < n; i++) b[i] = NULL;
  return b;
}

// The bucket index for a key slot (the sign mask keeps a user hash that returns a
// negative I64 in range).
I64 HmapBucket(Hmap *m, U8 *key, I64 n)
{
  return (m->hash(key) & 0x7FFFFFFFFFFFFFFF) % n;
}

// Initialise an empty map of `ksize`-byte keys to `vsize`-byte values, with the given
// key `hash`/`eq`. Required before any other call.
U0 HmapInit(Hmap *m, I64 ksize, I64 vsize,
            I64 (*hash)(U8 *), Bool (*eq)(U8 *, U8 *), U0 (*copy)(U8 *, U8 *))
{
  m->nbuckets = HMAP_INIT_BUCKETS;
  m->buckets = HmapNewBuckets(m->nbuckets);
  m->len = 0;
  m->ksize = ksize;
  m->vsize = vsize;
  m->hash = hash;
  m->eq = eq;
  m->copy = copy;
}

// Free every entry and the bucket array; return to the empty state. The `Hmap` struct
// is the caller's, as are any bytes a key/value pointer pointed at.
U0 HmapFree(Hmap *m)
{
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    U8 *e = m->buckets[i];
    while (e != NULL) {
      U8 *next = HmapNext(e);
      Free(e);
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = NULL;
  m->nbuckets = 0;
  m->len = 0;
}

// Double the bucket count and re-link every entry into the new array. The entries are
// reused — only the chain heads move.
U0 HmapRehash(Hmap *m)
{
  I64 newn = m->nbuckets * 2;
  U8 **nb = HmapNewBuckets(newn);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    U8 *e = m->buckets[i];
    while (e != NULL) {
      U8 *next = HmapNext(e);
      I64 b = HmapBucket(m, HmapKeyOf(e), newn);
      HmapSetNext(e, nb[b]);
      nb[b] = e;
      e = next;
    }
  }
  Free(m->buckets);
  m->buckets = nb;
  m->nbuckets = newn;
}

// Insert `key` (its `ksize` bytes are copied in), or find it if already present.
// Returns a pointer to the value slot — the caller writes the value through it:
// `*(I64 *)HmapPut(&m, &k) = 42;`. A fresh slot's value bytes are uninitialised.
U8 *HmapPut(Hmap *m, U8 *key)
{
  I64 b = HmapBucket(m, key, m->nbuckets);
  U8 *e = m->buckets[b];
  while (e != NULL) {
    if (m->eq(HmapKeyOf(e), key)) return HmapValOf(m, e);
    e = HmapNext(e);
  }
  U8 *node = MAlloc(sizeof(U8 *) + m->ksize + m->vsize);
  m->copy(HmapKeyOf(node), key);
  HmapSetNext(node, m->buckets[b]);
  m->buckets[b] = node;
  m->len++;
  if (m->len > m->nbuckets) HmapRehash(m);  // keep the load factor near 1
  return HmapValOf(m, node);
}

// Look up `key`. Returns `(value slot, TRUE)` when present, else `(NULL, FALSE)` — the
// flag distinguishes a stored value from a miss.
(U8 *, Bool) HmapGet(Hmap *m, U8 *key)
{
  U8 *e = m->buckets[HmapBucket(m, key, m->nbuckets)];
  while (e != NULL) {
    if (m->eq(HmapKeyOf(e), key)) return HmapValOf(m, e), TRUE;
    e = HmapNext(e);
  }
  return NULL, FALSE;
}

// Is `key` present?
Bool HmapHas(Hmap *m, U8 *key)
{
  (U8 *_, Bool ok) = HmapGet(m, key);
  return ok;
}

// Remove `key`, freeing its entry. Returns TRUE if it was present.
Bool HmapDel(Hmap *m, U8 *key)
{
  I64 b = HmapBucket(m, key, m->nbuckets);
  U8 *e = m->buckets[b];
  U8 *prev = NULL;
  while (e != NULL) {
    if (m->eq(HmapKeyOf(e), key)) {
      if (prev == NULL) m->buckets[b] = HmapNext(e);
      else HmapSetNext(prev, HmapNext(e));
      Free(e);
      m->len--;
      return TRUE;
    }
    prev = e;
    e = HmapNext(e);
  }
  return FALSE;
}

// The number of key/value pairs.
I64 HmapLen(Hmap *m) { return m->len; }

// Collect every key into `out` (initialised here to the map's key size, so the caller
// just declares `Vec keys;`). Order is unspecified — bucket order. Free with `VecFree`.
U0 HmapKeys(Hmap *m, Vec *out)
{
  VecInit(out, m->ksize);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    U8 *e = m->buckets[i];
    while (e != NULL) {
      m->copy(VecPush(out), HmapKeyOf(e));   // type-correct key move into the new slot
      e = HmapNext(e);
    }
  }
}

// Collect every value into `out` (initialised here to the map's value size). Order is
// unspecified — bucket order, parallel to `HmapKeys`. Free with `VecFree`.
U0 HmapValues(Hmap *m, Vec *out)
{
  VecInit(out, m->vsize);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    U8 *e = m->buckets[i];
    while (e != NULL) {
      MemCpy(VecPush(out), HmapValOf(m, e), m->vsize);   // value bytes (heap to heap)
      e = HmapNext(e);
    }
  }
}

// Collect every key/value pair into `out` as a `[key | value]` block of `ksize + vsize`
// bytes (the entry's own contiguous layout) — key at offset 0, value at offset `ksize`.
// Initialised here; order is bucket order. Because the key sits at offset 0, the stock
// key comparators sort the entries by key directly: `VecSort(&out, &HmapStrCmp)`. Free
// with `VecFree`.
U0 HmapEntries(Hmap *m, Vec *out)
{
  VecInit(out, m->ksize + m->vsize);
  I64 i;
  for (i = 0; i < m->nbuckets; i++) {
    U8 *e = m->buckets[i];
    while (e != NULL) {
      MemCpy(VecPush(out), HmapKeyOf(e), m->ksize + m->vsize);   // key+value, contiguous
      e = HmapNext(e);
    }
  }
}

// Collect every key into `out` and sort it by `cmp` (a key-slot comparator, e.g.
// `&HmapStrCmp`/`&HmapI64Cmp`) — the map's contents in sorted key order. Look the value
// up per key with `HmapGet`. Free `out` with `VecFree`.
U0 HmapSortKeys(Hmap *m, Vec *out, I64 (*cmp)(U8 *, U8 *))
{
  HmapKeys(m, out);
  VecSort(out, cmp);
}

#endif
