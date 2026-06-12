// tuple_class_field.hc — tuple as a class field

#include <stdio.hh>
(I64, I64) DivMod(I64 a, I64 b) { return a/b, a%b; }
class Range { (I64, I64) span; I64 id; };
Range g;
g.span = DivMod(23, 5);
g.id = 1;
"#%d: %d rem %d\n", g.id, g.span[0], g.span[1];
