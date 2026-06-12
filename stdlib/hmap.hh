#ifndef _HMAP_HH
#define _HMAP_HH
// hmap.hh — `Hmap<K, V>`, an owning generic hash map.
//
// `Hmap<K, V>` uses separate chaining over a growing bucket array, and is monomorphized
// per (key, value) type. Keys and values are typed, so there are no casts. The key's
// hashing and equality are function pointers given at `HmapInit`, since HolyC can't
// derive them; each takes a `K *`:
//
//     Hmap<U8 *, I64> m;
//     HmapInit(&m, &HmapStrHash, &HmapStrEq);   // string keys
//     HmapPut(&m, "answer", 42);
//     v, ok := HmapGet(&m, "answer");           // `ok` distinguishes a stored 0 from a miss
//
// Stock key ops are provided: `HmapI64Hash`/`HmapI64Eq` for I64 keys, and
// `HmapStrHash`/`HmapStrEq` for `U8 *` string keys (content-hashed and content-compared).
// It is built on `<string.hh>` for string keys (`StrCmp`) and on `<vec.hh>` for
// `HmapKeys`/`HmapValues`. The implementation is pure HolyC and behaves identically on
// the interpreter and every backend. Include with `#include <hmap.hh>`.
//
// The caller owns the `Hmap`, and `HmapInit` is required before use. An `Hmap` owns its
// entries, so free it with `HmapFree`. A `U8 *` string key stores the pointer, so the
// key must outlive the map (string literals do).


#include <vec.hh>
#include <stdlib.hh>
#include <string.hh>

#define HMAP_INIT_BUCKETS 8

public class HmapEntry<type K, type V> {
  HmapEntry<K, V> *next; // a chain of entries in the same bucket
  K key;
  V val;
}

public class Hmap<type K, type V> {
  HmapEntry<K, V> **buckets; // heap array of `nbuckets` chain heads
  I64 nbuckets;
  I64 len;
  I64 (*hash)(K *key);
  Bool (*eq)(K *a, K *b);
}

// A key/value pair, the element type `HmapEntries` collects. Each is a copy, detached
// from the map's internal chain, with no `next` link. `key` sits at offset 0, so a
// `Vec<HmapKV>` can be `VecSort`ed by a key comparator (`&CmpStr` or `&CmpI64`).
public class HmapKV<type K, type V> {
  K key;
  V val;
}

// ---- stock key hash/eq (passed to HmapInit) ----

public I64 HmapI64Hash(I64 *k);
public Bool HmapI64Eq(I64 *a, I64 *b);
// String keys: the key is a `U8 *`, so the op takes `U8 **` and dereferences it. `Djb2`
// is the private djb2 helper defined in the implementation.
public I64 HmapStrHash(U8 **k);
public Bool HmapStrEq(U8 **a, U8 **b);

// ---- core ----
//
// The generic `Hmap` operations are templates the parser must register *before* any use
// site (generics are define-before-use), so they cannot be deferred to the end like an
// ordinary `.hc` implementation. They live in `<hmap.hc>`, included at the foot of this
// header — the C++ template-header idiom — so they are parsed eagerly with these
// declarations. The prototypes are listed here for the reader; the bodies are in the
// implementation file:
//
//   U0   HmapInit     <type K, type V>(Hmap<K, V> *m, I64 (*hash)(K *), Bool (*eq)(K *, K *));
//   U0   HmapFree     <type K, type V>(Hmap<K, V> *m);
//   U0   HmapPut      <type K, type V>(Hmap<K, V> *m, K key, V val);
//   (V, Bool) HmapGet <type K, type V>(Hmap<K, V> *m, K key);
//   Bool HmapHas      <type K, type V>(Hmap<K, V> *m, K key);
//   Bool HmapDel      <type K, type V>(Hmap<K, V> *m, K key);
//   I64  HmapLen      <type K, type V>(Hmap<K, V> *m);
//   U0   HmapKeys     <type K, type V>(Hmap<K, V> *m, Vec<K> *out);
//   U0   HmapValues   <type K, type V>(Hmap<K, V> *m, Vec<V> *out);
//   U0   HmapSortKeys <type K, type V>(Hmap<K, V> *m, Vec<K> *out, I64 (*cmp)(K *, K *));
//   U0   HmapEntries  <type K, type V>(Hmap<K, V> *m, Vec<HmapKV<K, V>> *out);

#include <hmap.hc>

#endif
