// A function returns a pointer into a global array.
I64 g_arr[5] = {10, 20, 30, 40, 50};

I64 *GetPtr(I64 idx) {
  return &g_arr[idx];
}

I64 *p = GetPtr(2);
"%d\n", *p;
*p = 99;
"%d\n", g_arr[2];
