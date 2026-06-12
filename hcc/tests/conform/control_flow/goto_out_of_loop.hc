// goto out of a loop.

#include <stdio.hh>
I64 i;
for (i = 0; i < 100; i++) {
  if (i == 5)
    goto done;
  "%d ", i;
}
done:
"\n";
"stopped at %d\n", i;
