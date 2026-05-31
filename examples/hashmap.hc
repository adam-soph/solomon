// hashmap.hc — a string -> I64 hash map with separate chaining, built on the
// heap. Demonstrates typed MAlloc (of classes and of byte buffers), Free,
// StrLen/StrCpy/StrCmp, pointer-linked nodes, an array-of-pointers field, and a
// class threaded through functions by pointer.

#define NBUCKETS 8

class Entry {
  U8 *key;
  I64 value;
  Entry *next;
}

class Map {
  Entry *buckets[NBUCKETS];
}

// djb2-style hash of a NUL-terminated string, reduced to a bucket index.
I64 Hash(U8 *s) {
  I64 h = 5381;
  I64 i = 0;
  while (s[i] != 0) {
    h = h * 33 + s[i];
    i++;
  }
  return (h & 0x7FFFFFFFFFFFFFFF) % NBUCKETS;
}

U0 MapInit(Map *m) {
  I64 i;
  for (i = 0; i < NBUCKETS; i++)
    m->buckets[i] = NULL;
}

U0 MapPut(Map *m, U8 *key, I64 value) {
  I64 b = Hash(key);
  Entry *e = m->buckets[b];
  while (e != NULL) {
    if (StrCmp(e->key, key) == 0) { // update an existing key
      e->value = value;
      return;
    }
    e = e->next;
  }
  // Prepend a fresh entry (its key is copied onto the heap).
  Entry *node = MAlloc(sizeof(Entry));
  node->key = MAlloc(StrLen(key) + 1);
  StrCpy(node->key, key);
  node->value = value;
  node->next = m->buckets[b];
  m->buckets[b] = node;
}

I64 MapGet(Map *m, U8 *key, I64 missing) {
  Entry *e = m->buckets[Hash(key)];
  while (e != NULL) {
    if (StrCmp(e->key, key) == 0)
      return e->value;
    e = e->next;
  }
  return missing;
}

U0 Main() {
  Map m;
  MapInit(&m);
  MapPut(&m, "one", 1);
  MapPut(&m, "two", 2);
  MapPut(&m, "three", 3);
  MapPut(&m, "two", 22); // overwrite
  "one=%d two=%d three=%d missing=%d\n",
      MapGet(&m, "one", -1), MapGet(&m, "two", -1), MapGet(&m, "three", -1),
      MapGet(&m, "four", -1);
}

Main;
