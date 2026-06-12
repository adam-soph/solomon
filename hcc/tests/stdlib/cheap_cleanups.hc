#include <errno.hh>
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  "%.1f %.1f %.1f\n", Fmax(2.5, 1.5), Fmin(2.5, 1.5), Fmax(5.0, NaN()); // fmin/fmax
  q, r := Div(-7, 2);                                   // div/ldiv, truncates to (-3,-1)
  "%d %d\n", q, r;
  "%d %d\n", StrNLen("hello", 3), StrNLen("hi", 9);     // strnlen
  PutChar('H'); PutChar('i'); PutChar('\n');             // putchar
  Puts("line");                                          // puts (+newline)
  "%s|%s\n", StrError(ECONNABORTED), StrError(ECANCELED); // new errno codes
}
Main;
