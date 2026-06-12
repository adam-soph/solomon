// macro_flags_three.hc — three flags combining with ||

#include <stdio.hh>
#define HAS_A 1
#define HAS_B 0
#define HAS_C 1

#if HAS_A || HAS_B || HAS_C
"at least one\n";
#endif

#if HAS_A && HAS_C
"A and C\n";
#endif

#if !HAS_B
"no B\n";
#endif
