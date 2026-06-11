// class_with_two_f64.hc — class holding two F64 fields
class Complex { F64 re; F64 im; };
Complex MkC(F64 r, F64 i) { Complex c; c.re = r; c.im = i; return c; }
Complex Add(Complex a, Complex b) { return MkC(a.re + b.re, a.im + b.im); }
Complex x = MkC(1.5, 2.5);
Complex y = MkC(3.0, 4.0);
Complex z = Add(x, y);
"%f %f\n", z.re, z.im;
