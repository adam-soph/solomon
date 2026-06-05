#ifndef _SORT_HC
#define _SORT_HC
// sort.hc — generic sorting and binary search over a contiguous array: the C
// `qsort`/`bsearch` pair. The element size is a parameter and the order is a
// caller-supplied comparator `I64 (*cmp)(U8 *a, U8 *b)` returning <0 / 0 / >0 (like
// `StrCmp`). Standalone (no other library dependency); pure HolyC, identical on the
// interpreter and every backend. Include with `#include <sort.hc>`.
//
//     I64 CmpI64(U8 *a, U8 *b)
//     { I64 x = *(I64 *)a, y = *(I64 *)b; return x < y ? -1 : x > y; }
//
//     I64 *a = MAlloc(5 * sizeof(I64));   // a HEAP buffer (see the caveat below)
//     a[0]=3; a[1]=1; a[2]=4; a[3]=1; a[4]=5;
//     Sort(a, 5, sizeof(I64), &CmpI64);
//     I64 k = 4; U8 *p = BSearch(&k, a, 5, sizeof(I64), &CmpI64);   // ptr, or NULL
//
// The comparator receives pointers to two elements (into the array's bytes); cast and
// dereference them. `BSearch`'s `key` is likewise a pointer to a key element. Sorting
// works through the array bytes (a byte-wise swap, so a pointer or class-value element
// moves correctly — the interpreter byte-serialises pointers through its `PtrTable`).
//
// `base` must be a **heap** buffer (`MAlloc`, or a `Vec`'s data): the interpreter
// byte-addresses heap blocks but not a cell-backed stack array, so a raw `I64 a[N]`
// local is not a valid base there (it works natively but diverges under `hci`). The
// `<vec.hc>` wrappers `VecSort`/`VecBSearch` are the usual entry points and stay on the
// heap automatically. `BSearch`'s `key`, read only at offset 0, may be a stack local.
//
// The sort is an introspective-ish quicksort: median-of-three pivot with an insertion
// -sort cutoff for small ranges. It is **not stable**. `O(n log n)` typical. The
// `<vec.hc>`/`<hmap.hc>` containers wrap these (`VecSort`/`VecBSearch`, `HmapSortKeys`).

#define SORT_CUTOFF 16   // ranges this small are insertion-sorted

// Pointer to element `i` of an `esize`-stride array.
U8 *SortAt(U8 *base, I64 i, I64 esize) { return base + i * esize; }

// Swap two `n`-byte elements in place, byte by byte (a scalar byte temp — no buffer,
// so it works through the interpreter's heap byte buffers and moves serialised
// pointer/class bytes verbatim).
U0 SortSwap(U8 *a, U8 *b, I64 n)
{
  I64 i = 0;
  while (i < n) {
    U8 t = a[i];
    a[i] = b[i];
    b[i] = t;
    i++;
  }
}

// Insertion-sort the inclusive range [lo, hi].
U0 SortInsertion(U8 *base, I64 lo, I64 hi, I64 esize, I64 (*cmp)(U8 *, U8 *))
{
  I64 i = lo + 1;
  while (i <= hi) {
    I64 j = i;
    while (j > lo && cmp(SortAt(base, j - 1, esize), SortAt(base, j, esize)) > 0) {
      SortSwap(SortAt(base, j - 1, esize), SortAt(base, j, esize), esize);
      j--;
    }
    i++;
  }
}

// Quicksort the inclusive range [lo, hi].
U0 SortQuick(U8 *base, I64 lo, I64 hi, I64 esize, I64 (*cmp)(U8 *, U8 *))
{
  if (hi - lo < SORT_CUTOFF) {
    if (lo < hi) SortInsertion(base, lo, hi, esize, cmp);
    return;
  }
  // Median-of-three of (lo, mid, hi) ordered into those slots, then the median
  // (now at mid) is moved to hi to serve as the pivot.
  I64 mid = lo + (hi - lo) / 2;
  if (cmp(SortAt(base, mid, esize), SortAt(base, lo, esize)) < 0)
    SortSwap(SortAt(base, mid, esize), SortAt(base, lo, esize), esize);
  if (cmp(SortAt(base, hi, esize), SortAt(base, lo, esize)) < 0)
    SortSwap(SortAt(base, hi, esize), SortAt(base, lo, esize), esize);
  if (cmp(SortAt(base, hi, esize), SortAt(base, mid, esize)) < 0)
    SortSwap(SortAt(base, hi, esize), SortAt(base, mid, esize), esize);
  SortSwap(SortAt(base, mid, esize), SortAt(base, hi, esize), esize);

  // Lomuto partition around the pivot at `hi`.
  U8 *pivot = SortAt(base, hi, esize);
  I64 i = lo - 1;
  I64 j = lo;
  while (j < hi) {
    if (cmp(SortAt(base, j, esize), pivot) <= 0) {
      i++;
      SortSwap(SortAt(base, i, esize), SortAt(base, j, esize), esize);
    }
    j++;
  }
  i++;
  SortSwap(SortAt(base, i, esize), SortAt(base, hi, esize), esize);

  SortQuick(base, lo, i - 1, esize, cmp);
  SortQuick(base, i + 1, hi, esize, cmp);
}

// Sort `n` elements of `esize` bytes at `base` in place, ordered by `cmp`.
U0 Sort(U8 *base, I64 n, I64 esize, I64 (*cmp)(U8 *, U8 *))
{
  if (n > 1) SortQuick(base, 0, n - 1, esize, cmp);
}

// Binary-search a sorted array for `key` (a pointer to a key element). Returns a
// pointer to a matching element, or NULL if absent.
U8 *BSearch(U8 *key, U8 *base, I64 n, I64 esize, I64 (*cmp)(U8 *, U8 *))
{
  I64 lo = 0;
  I64 hi = n - 1;
  while (lo <= hi) {
    I64 mid = lo + (hi - lo) / 2;
    I64 c = cmp(key, SortAt(base, mid, esize));
    if (c == 0) return SortAt(base, mid, esize);
    if (c < 0) hi = mid - 1;
    else lo = mid + 1;
  }
  return NULL;
}

#endif
