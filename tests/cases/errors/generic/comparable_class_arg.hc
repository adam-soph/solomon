//@ error: is not comparable
// A `comparable T` parameter rejects a class argument at instantiation: only scalar
// and pointer types are orderable with `<`/`>`.
U0 Cmp<comparable T>(T a, T b) { if (a < b) { } }

class P { I64 x; };

U0 Main()
{
  P p;
  P q;
  Cmp(p, q);
}
