// class_u16_field.hc — class with U16 fields

#include <stdio.hh>
class Word { U16 lo; U16 hi; };
Word w; w.lo = 0xABCD; w.hi = 0x1234;
"%d %d\n", w.lo, w.hi;
