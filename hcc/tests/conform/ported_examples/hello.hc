// hello.hc — the basics: prints, variable declarations, top-level statements.

#include <stdio.hh>
#include <stdlib.hh>
U0 Main()
{
  "Hello, World!\n";
  I64 x = 42, y = 0xFF;
  F64 ratio = 3.14;
  "x=%d y=%d ratio=%f\n", x, y, ratio;
}

// HolyC runs top-level statements directly, like a script.
Main;
