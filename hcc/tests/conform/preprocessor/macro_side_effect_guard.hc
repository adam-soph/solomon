// macro_side_effect_guard.hc — parenthesizing avoids precedence surprises

#include <stdio.hh>
#define MUL(a, b) ((a) * (b))
// Without parens: MUL_BAD(a,b) a * b would give: 2 + 3 * 4 + 1 = 15, not (2+3)*(4+1)=25
I64 result = MUL(2 + 3, 4 + 1);
"%d\n", result;
I64 r2 = MUL(3, 2 + 4);
"%d\n", r2;
