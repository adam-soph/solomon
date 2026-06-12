// discard_slot.hc — _ discards unwanted slots

#include <stdio.hh>
(U8 *, I64) Tagged(I64 n) { return "answer", n; }
(I64, I64, F64) Stats(I64 a, I64 b) { return a+b, a*b, (a+b)/2.0; }

tag, _ := Tagged(42);
"tag=%s\n", tag;

_, prod, _ := Stats(4, 6);
"prod=%d\n", prod;
