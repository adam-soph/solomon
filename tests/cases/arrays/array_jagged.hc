// Jagged access pattern: read corners of a 4x4 array.
I64 a[4][4];
I64 i, j;
for (i = 0; i < 4; i++)
  for (j = 0; j < 4; j++)
    a[i][j] = i * 10 + j;
"%d %d %d %d\n", a[0][0], a[0][3], a[3][0], a[3][3];
