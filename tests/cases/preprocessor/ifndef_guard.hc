// ifndef_guard.hc — #ifndef include guard pattern
#ifndef _FOO_DEFINED
#define _FOO_DEFINED
I64 foo_val = 7;
#endif

#ifndef _FOO_DEFINED
"should not appear\n";
#else
"guard worked val=%d\n", foo_val;
#endif
