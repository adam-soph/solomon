// sizeof_class.hc — sizeof a class equals sum of fields (natural alignment)

#include <stdio.hh>
class Triple { I64 a; I64 b; I64 c; };
"%d\n", sizeof(Triple);
