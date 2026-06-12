// Three-level nested brace initializer.

#include <stdio.hh>
class Pt { I64 x; I64 y; };
class Seg { Pt p; Pt q; };
class Shape { Seg s; I64 id; };
Shape sh = {{{1,2},{3,4}}, 7};
"%d %d %d %d %d\n", sh.s.p.x, sh.s.p.y, sh.s.q.x, sh.s.q.y, sh.id;
