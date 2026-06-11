#include <math.hc>
F64 ip;
F64 frac = Modf(3.75, &ip);
"%f\n", ip;
"%f\n", frac;
