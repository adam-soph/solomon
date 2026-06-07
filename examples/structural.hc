// structural.hc — anonymous aggregates and structural type compatibility.
//
// solomon's `class`/`union` types are **structural**: two types with the same
// signature — the same ordered (field name, field type) list and the same kind
// (class vs union) — are interchangeable, whatever their names. So a named class,
// an anonymous `class { ... }`, and a `typedef` of one can be assigned, passed, and
// returned across each other. Anonymous aggregates are first-class types: a
// variable, parameter, return, or field may have one.

// A named class, and a `typedef` of an anonymous class with the same signature.
class Pt { I64 x; I64 y; }
typedef class { I64 x; I64 y; } Coord;

// A parameter typed as an anonymous class — a `Pt` or a `Coord` may be passed.
I64 SumPt(class { I64 x; I64 y; } p) { return p.x + p.y; }

// A function returning an anonymous class; the result fits any same-signature type.
class { I64 x; I64 y; } MkPt(I64 a, I64 b)
{
  class { I64 x; I64 y; } r;
  r.x = a;
  r.y = b;
  return r;
}

// Two structurally identical, self-referential named classes.
class NodeA { I64 v; NodeA *next; }
class NodeB { I64 v; NodeB *next; }

// Compatibility holds through the pointer field, so a NodeA chain can be walked as
// a NodeB list (argument checking is by arity, the bodies match by shape).
I64 SumList(NodeB *head)
{
  I64 s = 0;
  while (head) { s += head->v; head = head->next; }
  return s;
}

// A `typedef` of an anonymous union.
typedef union { I64 i; F64 f; } Num;

U0 Main()
{
  // Named -> anonymous -> typedef alias -> named: all one shape.
  Pt p = {3, 4};                       // brace init of a named class
  class { I64 x; I64 y; } q = p;       // named -> anonymous
  Coord c = q;                         // anonymous -> typedef alias
  Pt back = {.y = 40, .x = 30};        // designated init, out of order
  "named->anon->typedef: %d %d %d\n", q.x + q.y, c.x + c.y, back.x + back.y;

  // Pass a named and a typedef value to the anonymous-parameter function.
  "args: %d %d\n", SumPt(p), SumPt(c);

  // Consume an anonymous return as a named type, then as a typedef.
  Pt m = MkPt(10, 20);
  Coord n = MkPt(5, 6);
  "anon-return: %d %d\n", m.x + m.y, n.x + n.y;

  // A NodeA chain, summed through a NodeB pointer.
  NodeA a2 = {2, NULL};
  NodeA a1 = {1, &a2};
  "list: %d\n", SumList(&a1);

  // Anonymous union -> typedef union alias, same signature.
  union { I64 i; F64 f; } u;
  u.i = 99;
  Num num = u;
  "union: %d\n", num.i;
}

Main;
