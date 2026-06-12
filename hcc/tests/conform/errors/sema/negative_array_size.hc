//@ error: array size cannot be negative
#include <stdlib.hh>
class A { I64 a[-1]; };

A g;

U0 Main() {}
