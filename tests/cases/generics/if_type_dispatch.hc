// if_type_dispatch.hc — if type / else if type dispatch
U0 Show<type T>(T x) {
  if type (T is I64)       "int %d\n", x;
  else if type (T is F64)  "flt %.2f\n", x;
  else if type (T is U8 *) "str %s\n", x;
  else                     "other\n";
}
Show(42);
Show(3.14);
Show("hello");
