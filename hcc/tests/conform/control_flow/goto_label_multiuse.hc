// Multiple labels in a function, selective goto.

#include <stdio.hh>
#include <stdlib.hh>
U0 Run(I64 choice)
{
  if (choice == 1)
    goto label1;
  if (choice == 2)
    goto label2;
  "default\n";
  return;
label1:
  "label1\n";
  return;
label2:
  "label2\n";
  return;
}
Run(1);
Run(2);
Run(3);
