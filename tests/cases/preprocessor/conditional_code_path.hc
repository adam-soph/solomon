// conditional_code_path.hc — conditional compilation choosing code paths
#define MODE_DOUBLE
// MODE_INC and MODE_PASS not defined

I64 Compute(I64 x) {
#if defined(MODE_INC)
  return x + 1;
#elif defined(MODE_DOUBLE)
  return x * 2;
#else
  return x;
#endif
}
"%d\n", Compute(5);
"%d\n", Compute(10);
