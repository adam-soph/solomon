// min_max_funcs.hc — functions operating on a class, returning min/max fields
class Range { I64 lo; I64 hi; };
I64 Clamp(Range r, I64 v) {
  if (v < r.lo) return r.lo;
  if (v > r.hi) return r.hi;
  return v;
}
Range r; r.lo = 10; r.hi = 20;
"%d %d %d\n", Clamp(r, 5), Clamp(r, 15), Clamp(r, 25);
