#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  U8 *v = Getenv("HCC_ENV");
  if (v != NULL) "got=%s\n", v; else "missing\n";
}
Main;
