// Value lookup of a specific variable passed to the child's environment (HCC_ENV).
// The variable is scoped to the spawned process, so there's no parallel-test race on
// the shared env.

#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  I64 i = 0;
  while (envp[i] != NULL) {
    if (StrNCmp(envp[i], "HCC_ENV=", 8) == 0) { "got=%s\n", envp[i] + 8; return; }
    i++;
  }
  "missing\n";
}
Main;
