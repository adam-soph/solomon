// args.hc — the command line via the implicit globals `ArgC`/`ArgV` (captured at
// program entry, like C's `argc`/`argv`). `ArgC` is the count; `ArgV` is a
// `U8 **` of NUL-terminated argument strings, `ArgV[0]` being the program itself.
// No #include needed — `ArgC`/`ArgV` are ambient (the compiler injects them).
//
// Run it with arguments to see them echoed:  ./args one two three

U0 Main()
{
  "argc=%d\n", ArgC;
  // Skip ArgV[0] (the program path, which isn't reproducible); echo the rest.
  I64 i;
  for (i = 1; i < ArgC; i++)
    "arg[%d]=%s\n", i, ArgV[i];
  if (ArgC <= 1)
    "(no extra args)\n";
}

Main;
