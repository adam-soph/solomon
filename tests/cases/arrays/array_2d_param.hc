// 2-D array passed to a function that sums a column.
#define ROWS 3
#define COLS 4

I64 ColSum(I64 m[ROWS][COLS], I64 col) {
  I64 s = 0, i;
  for (i = 0; i < ROWS; i++) s += m[i][col];
  return s;
}

I64 m[ROWS][COLS] = {{1,2,3,4},{5,6,7,8},{9,10,11,12}};
"%d %d %d %d\n", ColSum(m,0), ColSum(m,1), ColSum(m,2), ColSum(m,3);
