// tuples.hc — first-class tuples: multi-value returns, indexing, destructuring.
//
// A tuple type `(T0, T1, ...)` is a positional, structural aggregate. It can be
// returned, stored, typedef'd, indexed with `[k]`, and destructured. Slots are
// positional only — there are no names, even behind a typedef.

typedef (I64, I64) Pair;

// Multiple return values: `return a, b;` builds the tuple.
(I64, I64) DivMod(I64 a, I64 b)
{
  return a / b, a % b;
}

// Tuples flow through parameters and returns like any value; `p[k]` indexes a slot.
Pair Swap(Pair p)
{
  return p[1], p[0];
}

// A 3-tuple, mixing element types, returned by value.
(I64, I64, F64) Stats(I64 a, I64 b)
{
  return a + b, a * b, (a + b) / 2.0;
}

U0 Main()
{
  // Declaration-form destructuring: each slot binds a fresh typed variable.
  (I64 q, I64 r) = DivMod(17, 5);
  "17 / 5 = %d rem %d\n", q, r;

  // The parentheses are optional when every slot is typed — a type before each name
  // distinguishes this from an ordinary `I64 a, b` declaration list.
  I64 q2, I64 r2 = DivMod(23, 4);
  "23 / 4 = %d rem %d\n", q2, r2;

  // For same-typed slots the repeated type is optional too: `I64 a, b = <tuple>` is an
  // unpack (the right-hand side being a tuple is what tells it apart from a decl list).
  I64 q3, r3 = DivMod(30, 7);
  "30 / 7 = %d rem %d\n", q3, r3;

  // `T _` discards a slot.
  (I64 only, I64 _) = DivMod(20, 3);
  "20 / 3 = %d\n", only;

  // Store a tuple, index it, pass it through a function.
  Pair p = Swap(DivMod(9, 4));   // DivMod -> (2, 1); Swap -> (1, 2)
  "p[0]=%d p[1]=%d\n", p[0], p[1];

  // Assignment-form destructuring into existing variables from a simple source.
  I64 x;
  I64 y;
  (x, y) = p;
  "x=%d y=%d\n", x, y;

  // A 3-tuple with a float slot, accessed positionally.
  (I64 sum, I64 prod, F64 avg) = Stats(7, 3);
  "sum=%d prod=%d avg=%.1f\n", sum, prod, avg;
}

Main;
