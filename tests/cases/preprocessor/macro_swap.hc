// macro_swap.hc — SWAP macro using a temp variable (not possible without tmp)
// Use a function-macro that relies on a statement trick (comma expr style)
#define SWAP_I64(a, b) { I64 _t = (a); (a) = (b); (b) = _t; }
I64 x = 10, y = 20;
SWAP_I64(x, y);
"%d %d\n", x, y;
I64 p = 3, q = 7;
SWAP_I64(p, q);
"%d %d\n", p, q;
