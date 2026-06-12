// sort.hc — generic `<sort.hc>` over a `<vec.hh>` `Vec<T>`: one quicksort drives both
// an `I64` vector and a string vector, each ordered by a stock comparator, plus a
// binary search over the sorted result. The element type is the only thing that
// changes — the sort itself is element-size agnostic, monomorphized per `T`.

#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <vec.hh>
#include <vec.hh>    // Vec<T> + VecSort/VecBSearch (pulls in <sort.hc>)
#include <string.hh>   // CmpStr (the U8 * string comparator)

U0 Main()
{
  // --- an I64 vector ---
  Vec<I64> v;
  VecInit(&v);
  I64 nums[] = {5, 2, 8, 1, 9, 3, 7, 4, 6, 0};
  I64 i;
  for (i = 0; i < 10; i++) VecPush(&v, nums[i]);

  VecSort(&v, &CmpI64);              // CmpI64: stock I64 comparator from <sort.hc>
  for (i = 0; i < VecLen(&v); i++) "%d ", VecAt(&v, i);
  "\n";

  // Binary search the sorted vector (returns an index, or -1).
  I64 k = 7;
  "find 7 -> %d\n", VecBSearch(&v, &k, &CmpI64);
  k = 100;
  "find 100 -> %d\n", VecBSearch(&v, &k, &CmpI64);
  VecFree(&v);

  // --- a string vector, same sort ---
  Vec<U8 *> s;
  VecInit(&s);
  VecPush(&s, "pear");
  VecPush(&s, "apple");
  VecPush(&s, "cherry");
  VecPush(&s, "banana");

  VecSort(&s, &CmpStr);             // CmpStr: stock U8 * comparator from <string.hh>
  for (i = 0; i < VecLen(&s); i++) "%s ", VecAt(&s, i);
  "\n";
  VecFree(&s);
}

Main;
