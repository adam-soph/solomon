// envp is a NULL-terminated U8 ** of "KEY=VALUE" strings. These are structural
// invariants that hold for any real process environment: it is non-empty and every
// entry has a '='. So the check is deterministic without depending on a specific
// variable.

#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  I64 i = 0, all_kv = 1;
  while (envp[i] != NULL) {
    if (StrChr(envp[i], '=') == NULL) all_kv = 0;
    i++;
  }
  "nonempty=%d all_kv=%d\n", i > 0, all_kv;
}
Main;
