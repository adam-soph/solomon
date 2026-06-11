// do-while loop.
I64 i = 0;
do {
  "%d\n", i;
  i++;
} while (i < 4);

// do-while executes at least once even when condition is false initially.
I64 x = 10;
do {
  "ran once\n";
} while (x < 5);
