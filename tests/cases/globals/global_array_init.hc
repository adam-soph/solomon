// Global array with an initializer; sum it.
I64 g_data[5] = {10, 20, 30, 40, 50};

I64 Sum() {
  I64 s = 0, i;
  for (i = 0; i < 5; i++) s += g_data[i];
  return s;
}

"%d\n", Sum();
