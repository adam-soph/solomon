// own_flag_conditional.hc — conditional on a user-defined flag (not platform)
#define USE_FAST_PATH 1

I64 Process(I64 x) {
#if USE_FAST_PATH
  return x * 2;
#else
  I64 i, r = 0;
  for (i = 0; i < 2; i++) r += x;
  return r;
#endif
}
"%d\n", Process(7);
"%d\n", Process(21);
