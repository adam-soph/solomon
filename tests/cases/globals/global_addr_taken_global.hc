// A top-level scalar whose address is taken must stay a global (a pointer aliases it); a
// function writing through that pointer is visible at the top level.
I64 g = 10;
U0 SetTo(I64 *p, I64 v) { *p = v; }
"g=%d\n", g;
SetTo(&g, 42);
"g=%d\n", g;
g += 8;
"g=%d\n", g;
