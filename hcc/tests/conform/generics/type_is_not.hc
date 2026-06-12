// type_is_not.hc — if type (T is not ...) negation

#include <stdio.hh>
U8 *Kind<type T>() {
  if type (T is not F64) return "int";
  else return "float";
}
"%s\n", Kind<I64>();
"%s\n", Kind<F64>();
"%s\n", Kind<U8 *>();
