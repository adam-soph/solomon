//@ args: alpha beta gamma
// args.hc — the command line via `argc`/`argv`, read at **top-level scope**. At the top
// level (outside any function) `argc`/`argv` are the command line: `argc` is the count and
// `argv` is a `U8 **` of NUL-terminated argument strings, `argv[0]` being the program
// itself. Inside a function the same names mean the variadic arguments instead (see
// `varargs.hc`), so command-line handling lives here at the top level, not in a function.
// No #include needed — `argc`/`argv` are ambient (the compiler injects them).
//
// Run it with arguments to see them echoed:  ./args one two three


#include <stdio.hh>
"argc=%d\n", argc;
// Skip argv[0] (the program path, which isn't reproducible); echo the rest.
I64 i;
for (i = 1; i < argc; i++)
  "arg[%d]=%s\n", i, argv[i];
if (argc <= 1)
  "(no extra args)\n";
