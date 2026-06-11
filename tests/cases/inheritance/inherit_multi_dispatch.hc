// inherit_multi_dispatch.hc — dispatch over 4 shape kinds, sum areas
#define SQ  0
#define CIR 1
#define REC 2
#define TRP 3

class Sh { I64 kind; };
class Sq  : Sh { F64 side; };
class Cir : Sh { F64 r; };
class Rec : Sh { F64 w; F64 h; };
class Trp : Sh { F64 base; F64 ht; };

F64 Ar(Sh *s) {
  switch (s->kind) {
    case SQ:  { Sq  *q = (Sq*)s;  return q->side * q->side; }
    case CIR: { Cir *c = (Cir*)s; return 3.14159265358979 * c->r * c->r; }
    case REC: { Rec *r = (Rec*)s; return r->w * r->h; }
    case TRP: { Trp *t = (Trp*)s; return 0.5 * t->base * t->ht; }
  }
  return 0.0;
}

Sq  sq; sq.kind = SQ;  sq.side = 3.0;
Cir ci; ci.kind = CIR; ci.r = 2.0;
Rec re; re.kind = REC; re.w = 5.0; re.h = 2.0;
Trp tr; tr.kind = TRP; tr.base = 4.0; tr.ht = 3.0;

F64 total = Ar((Sh*)&sq) + Ar((Sh*)&ci) + Ar((Sh*)&re) + Ar((Sh*)&tr);
"%f\n", total;
