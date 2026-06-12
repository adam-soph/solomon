#include <stdio.hh>
#include <stdlib.hh>
U8 *Kind<type T>(T x) {
  if type (T is F64) return "float";
  if type (T is not I64) return "other";
  else return "int";
}
U0 Main() { "%s %s %s\n", Kind(1.5), Kind(42), Kind("hi"); }
Main;
