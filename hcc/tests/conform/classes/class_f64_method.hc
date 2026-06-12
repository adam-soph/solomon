// class_f64_method.hc — method-by-convention computing F64 from class

#include <stdio.hh>
class Line { F64 dx; F64 dy; };
F64 LenSq(Line *l) { return l->dx * l->dx + l->dy * l->dy; }
Line ln; ln.dx = 3.0; ln.dy = 4.0;
"%f\n", LenSq(&ln);
