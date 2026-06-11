// 3-D array: fill and read back one plane.
I64 a[2][3][4];
I64 i, j, k;
for (i = 0; i < 2; i++)
  for (j = 0; j < 3; j++)
    for (k = 0; k < 4; k++)
      a[i][j][k] = i * 100 + j * 10 + k;
"%d %d %d\n", a[0][0][0], a[1][2][3], a[0][2][1];
