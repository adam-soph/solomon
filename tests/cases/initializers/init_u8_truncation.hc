// Storing into U8 truncates to one byte.
U8 a[3] = {300, 255, 1};
"%d %d %d\n", (I64)a[0], (I64)a[1], (I64)a[2];
