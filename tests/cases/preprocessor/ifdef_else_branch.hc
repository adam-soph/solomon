// ifdef_else_branch.hc — #ifdef else branch taken
// RELEASE is not defined
#ifdef RELEASE
"release build\n";
#else
"debug build\n";
#endif
I64 x = 42;
"%d\n", x;
