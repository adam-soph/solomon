#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
#include <vec.hh>
U0 Main() {
  Vec<U8 *> env;
  Environ(&env);
  I64 i, all_kv = 1;
  for (i = 0; i < VecLen(&env); i++)
    if (StrChr(VecAt(&env, i), '=') == NULL) all_kv = 0;
  "count>0=%d all_kv=%d\n", VecLen(&env) > 0, all_kv;
  VecFree(&env);
}
Main;
