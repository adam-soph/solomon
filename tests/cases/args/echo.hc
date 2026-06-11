//@ args: one two three
// Command-line args via top-level argc/argv. argv[0] is skipped (not reproducible).
"argc=%d\n", argc;
I64 i;
for (i = 1; i < argc; i++)
  "arg[%d]=%s\n", i, argv[i];
