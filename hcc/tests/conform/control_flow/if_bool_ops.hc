// Boolean expressions with and/or/not.

#include <stdio.hh>
Bool a = TRUE, b = FALSE;
if (a && !b)
  "T and not F\n";
if (!a || b)
  "should not print\n";
if (a || b)
  "T or F\n";
if (!(a && b))
  "not (T and F)\n";
