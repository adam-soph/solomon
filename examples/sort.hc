// sort.hc — generic `<sort.hc>` over a `<vec.hc>` Vec: one quicksort drives both an
// I64 vector and a string vector, each ordered by its own comparator, plus a binary
// search over the sorted result. The comparator `I64 (*cmp)(U8 *, U8 *)` is the only
// per-type code — the sort itself is element-size agnostic.

#include <vec.hc>    // Vec + VecSort/VecBSearch (pulls in <sort.hc>)
#include <cstr.hc>   // for StrCmp in the string comparator

// Ascending I64 order. The comparator gets pointers to two elements.
I64 CmpI64(U8 *a, U8 *b)
{
  I64 x = *(I64 *)a, y = *(I64 *)b;
  return x < y ? -1 : x > y;
}

// Lexicographic string order (elements are `U8 *`, so dereference the slot).
I64 CmpStr(U8 *a, U8 *b) { return StrCmp(*(U8 **)a, *(U8 **)b); }

U0 Main()
{
  // --- an I64 vector ---
  Vec v;
  VecInit(&v, sizeof(I64));
  I64 nums[] = {5, 2, 8, 1, 9, 3, 7, 4, 6, 0};
  I64 i;
  for (i = 0; i < 10; i++) *(I64 *)VecPush(&v) = nums[i];

  VecSort(&v, &CmpI64);
  for (i = 0; i < v.len; i++) "%d ", *(I64 *)VecAt(&v, i);
  "\n";

  // Binary search the sorted vector (returns an index, or -1).
  I64 k = 7;
  "find 7 -> %d\n", VecBSearch(&v, &k, &CmpI64);
  k = 100;
  "find 100 -> %d\n", VecBSearch(&v, &k, &CmpI64);
  VecFree(&v);

  // --- a string vector, same sort ---
  Vec s;
  VecInit(&s, sizeof(U8 *));
  *(U8 **)VecPush(&s) = "pear";
  *(U8 **)VecPush(&s) = "apple";
  *(U8 **)VecPush(&s) = "cherry";
  *(U8 **)VecPush(&s) = "banana";

  VecSort(&s, &CmpStr);
  for (i = 0; i < s.len; i++) "%s ", *(U8 **)VecAt(&s, i);
  "\n";
  VecFree(&s);
}

Main;
