#ifndef _SORT_HC
#define _SORT_HC
// sort.hc — generic sorting and binary search over a contiguous typed array.
//
// `Sort<T>` and `BSearch<T>` are the `qsort`/`bsearch` pair, monomorphized per element
// type — typed throughout, with no element-size bookkeeping or casts. The order is a
// caller-supplied comparator `I64 (*cmp)(T *a, T *b)` that returns <0, 0, or >0 (like
// `StrCmp`), receiving pointers to two elements. The module is standalone, with no other
// library dependency. It is pure HolyC and behaves identically on the interpreter and
// every backend. Include with `#include <sort.hc>`.
//
//     I64 CmpI64(I64 *a, I64 *b) { return *a < *b ? -1 : *a > *b; }   // a stock comparator
//
//     I64 *a = MAlloc(5 * sizeof(I64));   // a HEAP buffer (see the caveat below)
//     a[0]=3; a[1]=1; a[2]=4; a[3]=1; a[4]=5;
//     Sort(a, 5, &CmpI64);                // T = I64 inferred from `a`
//     I64 k = 4; I64 *p = BSearch(&k, a, 5, &CmpI64);   // ptr to a match, or NULL
//
// The comparator's `T = I64` is inferred from the array argument. `BSearch`'s `key` is a
// pointer to a key element. Elements move with a typed swap (`T t = *a; *a = *b; *b = t`),
// so pointer and class-value elements move correctly. (In the interpreter a class element
// is byte-serialised through its heap buffer.)
//
// `base` must be a heap buffer: `MAlloc`'d memory, or a `Vec`'s data. The interpreter
// byte-addresses heap blocks but not a cell-backed stack array, so a raw `I64 a[N]` local
// is not a valid base there — it works natively but diverges under `hcc -i`. The
// `<vec.hc>` wrappers `VecSort`/`VecBSearch` are the usual entry points and stay on the
// heap automatically. `BSearch`'s `key` is read only as a single element, so it may be a
// stack local.
//
// The sort is a median-of-three quicksort with an insertion-sort cutoff for small ranges.
// It is not stable. Typical cost is `O(n log n)`. The `<vec.hc>` and `<hmap.hc>`
// containers wrap these as `VecSort`/`VecBSearch` and `HmapSortKeys`.

#define SORT_CUTOFF 16   // ranges this small are insertion-sorted

// Stock scalar element comparators, for the `cmp` argument to
// `Sort`/`BSearch`/`VecSort`/`HmapSortKeys`. Each receives pointers to two elements.
// (`CmpStr`, for a `U8 *` string-pointer element, lives in `<cstr.hc>` next to `StrCmp`.)
public I64 CmpI64(I64 *a, I64 *b) { return *a < *b ? -1 : *a > *b; }
public I64 CmpU64(U64 *a, U64 *b) { return *a < *b ? -1 : *a > *b; }
public I64 CmpF64(F64 *a, F64 *b) { return *a < *b ? -1 : *a > *b; }

// Swap two elements in place.
U0 SortSwap<type T>(T *a, T *b) { T t = *a; *a = *b; *b = t; }

// Insertion-sort the inclusive range [lo, hi].
U0 SortInsertion<type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *))
{
  I64 i = lo + 1;
  while (i <= hi) {
    I64 j = i;
    while (j > lo && cmp(&base[j - 1], &base[j]) > 0) {
      SortSwap<T>(&base[j - 1], &base[j]);
      j--;
    }
    i++;
  }
}

// Quicksort the inclusive range [lo, hi].
U0 SortQuick<type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *))
{
  if (hi - lo < SORT_CUTOFF) {
    if (lo < hi) SortInsertion<T>(base, lo, hi, cmp);
    return;
  }
  // Median-of-three of (lo, mid, hi) ordered into those slots, then the median (now at
  // mid) is moved to hi to serve as the pivot.
  I64 mid = lo + (hi - lo) / 2;
  if (cmp(&base[mid], &base[lo]) < 0) SortSwap<T>(&base[mid], &base[lo]);
  if (cmp(&base[hi], &base[lo]) < 0) SortSwap<T>(&base[hi], &base[lo]);
  if (cmp(&base[hi], &base[mid]) < 0) SortSwap<T>(&base[hi], &base[mid]);
  SortSwap<T>(&base[mid], &base[hi]);

  // Lomuto partition around the pivot at `hi`, which stays put during the loop.
  T *pivot = &base[hi];
  I64 i = lo - 1;
  I64 j = lo;
  while (j < hi) {
    if (cmp(&base[j], pivot) <= 0) {
      i++;
      SortSwap<T>(&base[i], &base[j]);
    }
    j++;
  }
  i++;
  SortSwap<T>(&base[i], &base[hi]);

  SortQuick<T>(base, lo, i - 1, cmp);
  SortQuick<T>(base, i + 1, hi, cmp);
}

// Sort `n` elements at `base` in place, ordered by `cmp`.
U0 Sort<type T>(T *base, I64 n, I64 (*cmp)(T *, T *))
{
  if (n > 1) SortQuick<T>(base, 0, n - 1, cmp);
}

// Binary-search a sorted array for `key`, a pointer to a key element. Returns a pointer
// to a matching element, or NULL if absent.
T *BSearch<type T>(T *key, T *base, I64 n, I64 (*cmp)(T *, T *))
{
  I64 lo = 0;
  I64 hi = n - 1;
  while (lo <= hi) {
    I64 mid = lo + (hi - lo) / 2;
    I64 c = cmp(key, &base[mid]);
    if (c == 0) return &base[mid];
    if (c < 0) hi = mid - 1;
    else lo = mid + 1;
  }
  return NULL;
}

#endif
