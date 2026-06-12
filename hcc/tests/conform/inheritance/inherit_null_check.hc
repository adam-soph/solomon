// inherit_null_check.hc — base pointer may be NULL; guard before dispatch

#include <stdio.hh>
class Base { I64 val; };
I64 SafeGet(Base *b) { if (b == NULL) return -1; return b->val; }
Base b; b.val = 5;
"%d\n", SafeGet(&b);
"%d\n", SafeGet(NULL);
