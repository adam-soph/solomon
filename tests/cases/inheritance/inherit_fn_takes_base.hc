// inherit_fn_takes_base.hc — function takes Base *, dispatches by kind, returns I64
class Expr { I64 kind; };
class Num  : Expr { I64 v; };
class Add  : Expr { I64 lv; I64 rv; };

I64 Eval(Expr *e) {
  switch (e->kind) {
    case 0: return ((Num *)e)->v;
    case 1: return ((Add *)e)->lv + ((Add *)e)->rv;
  }
  return 0;
}

Num n; n.kind = 0; n.v = 42;
Add a; a.kind = 1; a.lv = 10; a.rv = 32;
"%d\n", Eval((Expr *)&n);
"%d\n", Eval((Expr *)&a);
