// args.hc — the command line via the implicit globals `argc`/`argv` (captured at
// program entry, like C's `argc`/`argv`). `argc` is the count; `argv` is a
// `U8 **` of NUL-terminated argument strings, `argv[0]` being the program itself.
// No #include needed — `argc`/`argv` are ambient (the compiler injects them).
//
// Run it with arguments to see them echoed:  ./args one two three

U0 Main()
{
  "argc=%d\n", argc;
  // Skip argv[0] (the program path, which isn't reproducible); echo the rest.
  I64 i;
  for (i = 1; i < argc; i++)
    "arg[%d]=%s\n", i, argv[i];
  if (argc <= 1)
    "(no extra args)\n";
}

Main;
