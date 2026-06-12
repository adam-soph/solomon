// copy_then_mutate.hc — copy a class, mutate the copy, original unchanged

#include <stdio.hh>
class Point { I64 x; I64 y; };
Point orig; orig.x = 10; orig.y = 20;
Point copy = orig;
copy.x = 99; copy.y = 88;
"%d %d\n", orig.x, orig.y;
"%d %d\n", copy.x, copy.y;
