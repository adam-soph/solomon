// try_return_from_fn.hc — function with try that returns normally
I64 SafeDouble(I64 x) {
  try {
    if (x < 0) throw(-1);
    return x * 2;
  } catch {
    return 0;
  }
}
"%d\n", SafeDouble(5);
"%d\n", SafeDouble(-3);
"%d\n", SafeDouble(10);
