// wordcount.hc — a word-frequency analyzer that leans hard on the generic stdlib with
// **inferred** type arguments throughout (no `<...>` at any call site). It exercises:
//   * `Hmap<U8 *, I64>`  — word -> count          (string keys)
//   * `Hmap<I64, I64>`   — word length -> #words   (integer keys)
//   * `Vec<U8 *>`        — the tokens, and the sorted key list
//   * `Vec<I64>`         — the values, for an order-independent sum
//   * `Vec<HmapKV<U8 *, I64>>` — entries, sorted by a custom comparator
// Every `VecPush`/`VecAt`/`HmapGet`/`HmapPut`/… below infers its type args from the
// receiver, so the generic machinery is doing real work on every line.

#include <ctype.hh>
#include <hmap.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <vec.hh>
#include <hmap.hh>    // Hmap<K,V>, HmapKV, the stock string/I64 key ops, CmpStr (via cstr)
#include <ctype.hh>   // IsAlpha / ToLower

// Copy the lowercased word text[beg..fin) into a fresh heap string and append it.
U0 PushWord(Vec<U8 *> *words, U8 *text, I64 beg, I64 fin)
{
  I64 n = fin - beg;
  U8 *w = MAlloc(n + 1);
  I64 i;
  for (i = 0; i < n; i++)
    w[i] = ToLower(text[beg + i]);
  w[n] = 0;
  VecPush(words, w);                  // T = U8 * inferred from `words`
}

// Split `text` into lowercased alphabetic words, appended to `words`.
U0 Tokenize(U8 *text, Vec<U8 *> *words)
{
  I64 i = 0, beg = -1;
  while (text[i]) {
    if (IsAlpha(text[i])) {
      if (beg < 0) beg = i;       // word begins
    } else if (beg >= 0) {
      PushWord(words, text, beg, i);
      beg = -1;                     // word ends
    }
    i++;
  }
  if (beg >= 0) PushWord(words, text, beg, i);
}

// Order entries by descending count, then ascending key. The comparator receives
// pointers to two `HmapKV` elements.
I64 CmpByCount(U8 *a, U8 *b)
{
  HmapKV<U8 *, I64> *x = (HmapKV<U8 *, I64> *)a;
  HmapKV<U8 *, I64> *y = (HmapKV<U8 *, I64> *)b;
  if (x->val != y->val) return x->val < y->val ? 1 : -1;
  return StrCmp(x->key, y->key);
}

U0 Main()
{
  U8 *text = "The quick brown fox jumps over the lazy dog. The dog barks, the fox runs, and the quick fox jumps again over the lazy dog!";

  // --- tokenize ---
  Vec<U8 *> words;
  VecInit(&words);
  Tokenize(text, &words);

  // --- count word frequencies ---
  Hmap<U8 *, I64> counts;
  HmapInit(&counts, &HmapStrHash, &HmapStrEq);
  I64 i;
  for (i = 0; i < VecLen(&words); i++) {
    U8 *w = VecAt(&words, i);                 // T = U8 *
    c, found := HmapGet(&counts, w); // (I64, Bool)
    HmapPut(&counts, w, found ? c + 1 : 1);    // value from the receiver
  }
  "tokens=%d distinct=%d\n", VecLen(&words), HmapLen(&counts);

  // --- alphabetical listing (sorted keys, look each count back up) ---
  Vec<U8 *> keys;
  HmapSortKeys(&counts, &keys, &CmpStr);
  "by word:\n";
  for (i = 0; i < VecLen(&keys); i++) {
    U8 *k = VecAt(&keys, i);
    c, _f := HmapGet(&counts, k);
    "  %s %d\n", k, c;
  }

  // --- top 3 by frequency (entries sorted by the custom comparator) ---
  Vec<HmapKV<U8 *, I64>> ents;
  HmapEntries(&counts, &ents);
  VecSort(&ents, &CmpByCount);
  "top 3:\n";
  for (i = 0; i < VecLen(&ents) && i < 3; i++) {
    HmapKV<U8 *, I64> *kv = VecRef(&ents, i);  // T = HmapKV<U8 *, I64>
    "  %s x%d\n", kv->key, kv->val;
  }

  // --- length histogram: distinct words grouped by length (I64 -> I64 map) ---
  Hmap<I64, I64> bylen;
  HmapInit(&bylen, &HmapI64Hash, &HmapI64Eq);
  for (i = 0; i < VecLen(&keys); i++) {
    I64 len = StrLen(VecAt(&keys, i));
    n, f := HmapGet(&bylen, len);
    HmapPut(&bylen, len, f ? n + 1 : 1);
  }
  Vec<I64> lens;
  HmapSortKeys(&bylen, &lens, &CmpI64);
  "by length:\n";
  for (i = 0; i < VecLen(&lens); i++) {
    I64 l = VecAt(&lens, i);
    n, _f := HmapGet(&bylen, l);
    "  len %d: %d\n", l, n;
  }

  // --- order-independent check: sum of all counts == token total ---
  Vec<I64> vals;
  HmapValues(&counts, &vals);
  I64 sum = 0;
  for (i = 0; i < VecLen(&vals); i++) sum += VecAt(&vals, i);
  "sum=%d\n", sum;

  // --- cleanup ---
  for (i = 0; i < VecLen(&words); i++) Free(VecAt(&words, i));
  VecFree(&words);
  VecFree(&keys);
  VecFree(&ents);
  VecFree(&lens);
  VecFree(&vals);
  HmapFree(&counts);
  HmapFree(&bylen);
}

Main;
