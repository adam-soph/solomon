// decl_infer_whole_tuple.hc — := with one name binds a whole tuple

#include <stdio.hh>
(I64, I64) DivMod(I64 a, I64 b) { return a/b, a%b; }
t := DivMod(22, 7);
"q=%d r=%d\n", t[0], t[1];
