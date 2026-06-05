// tuples.hc — a full tour of first-class tuples and their quirks.
//
// A tuple type `(T0, ..., Tn)` (n >= 2) is a **positional, structural** aggregate:
//   * positional only — no named slots, even behind a `typedef`;
//   * not nestable — a tuple can't be an element of another tuple;
//   * a tuple *literal* `(a, b)` is only valid as a variable initializer or a
//     `return` value — not as a function argument (pass a tuple *variable* instead);
//   * indexing `t[k]` needs a literal constant `k`;
//   * `:=` declares fresh variables with inferred types — *one* name infers that
//     variable's type from the right-hand side (`n := 5`), *two or more* unpack a tuple
//     (`a, b := pair`). It always declares, so it can't reassign existing variables.

#include <vec.hc>    // tuples as generic-container elements
#include <hmap.hc>   // unpacking a generic function's tuple return

typedef (I64, I64) Pair;

// Multiple return values: `return a, b;` builds the tuple.
(I64, I64) DivMod(I64 a, I64 b) { return a / b, a % b; }

// Tuples flow through parameters and returns by value; `p[k]` indexes a slot.
Pair Swap(Pair p) { return p[1], p[0]; }

// Mixed element types, including a string slot.
(U8 *, I64) Tagged(I64 n) { return "answer", n; }

// A 3-tuple with a float slot.
(I64, I64, F64) Stats(I64 a, I64 b) { return a + b, a * b, (a + b) / 2.0; }

// A tuple as a class field.
class Range { (I64, I64) span; I64 id; }

U0 Main()
{
  // `:=` with ONE name is an inferred-type declaration — the type comes from the
  // right-hand side: a literal, an expression, a call result, even a whole tuple.
  count := 5;                 // I64
  ratio := 1.5;               // F64
  label := "items";           // U8 *
  doubled := count * 2;       // arithmetic -> I64
  whole := DivMod(22, 7);     // a whole tuple bound to one variable
  "infer: %d %.1f %s %d (%d,%d)\n", count, ratio, label, doubled, whole[0], whole[1];

  // `:=` with TWO OR MORE names unpacks a tuple — each name gets its element's type.
  q, r := DivMod(17, 5);
  "divmod: %d rem %d\n", q, r;

  // A 3-tuple, mixed types, `%.1f` float slot.
  sum, prod, avg := Stats(7, 3);
  "stats: sum=%d prod=%d avg=%.1f\n", sum, prod, avg;

  // `_` discards a slot; the kept slot is a string.
  tag, _ := Tagged(42);
  "tagged: %s\n", tag;

  // Bind a WHOLE tuple to one variable (a typed decl — `:=` is for 2+ names), then
  // index its slots with literal constants.
  (I64, I64) t = DivMod(20, 6);
  "indexed: t[0]=%d t[1]=%d\n", t[0], t[1];

  // A tuple flows through a function by value. Pass a tuple *variable* (a literal
  // `(8, 9)` isn't allowed as a call argument), then unpack the result.
  Pair in = (8, 9);
  a, b := Swap(in);
  "swapped: %d %d\n", a, b;

  // typedef'd tuple, returned from a call, stored and indexed.
  Pair pr = Swap(DivMod(9, 4));   // DivMod -> (2, 1); Swap -> (1, 2)
  "pair: %d %d\n", pr[0], pr[1];

  // A tuple as a class field, assigned from a tuple-returning call.
  Range g;
  g.span = DivMod(23, 5);
  g.id = 1;
  "range #%d: %d rem %d\n", g.id, g.span[0], g.span[1];

  // Tuples as elements of a generic container (push tuple *variables*).
  Vec<(I64, I64)> v;
  VecInit(&v);
  (I64, I64) e0 = (1, 10);
  (I64, I64) e1 = (2, 20);
  VecPush(&v, e0);
  VecPush(&v, e1);
  (I64, I64) got = VecAt(&v, 1);
  "vec: key=%d val=%d\n", got[0], got[1];
  VecFree(&v);

  // `:=` through a *generic* call: `HmapGet` returns `(V, Bool)`; the element types are
  // inferred from the monomorphized instance's return type.
  Hmap<I64, I64> m;
  HmapInit(&m, &HmapI64Hash, &HmapI64Eq);
  HmapPut(&m, 7, 49);
  val, ok := HmapGet(&m, 7);
  miss, found := HmapGet(&m, 8);
  "hmap: 7->%d (ok=%d)  8 found=%d\n", val, ok, found;
  HmapFree(&m);
}

Main;
