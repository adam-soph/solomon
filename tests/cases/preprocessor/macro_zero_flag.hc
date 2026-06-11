// macro_zero_flag.hc — #if 0 skips code; #if 1 includes it
I64 x = 5;
#if 1
x = x + 1;
#endif
#if 0
x = x * 100;
#endif
"%d\n", x;
