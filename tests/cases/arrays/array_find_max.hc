// Find the maximum element and its index.
I64 a[7] = {3, 7, 2, 9, 1, 8, 4};
I64 max_val = a[0], max_idx = 0, i;
for (i = 1; i < 7; i++) {
  if (a[i] > max_val) { max_val = a[i]; max_idx = i; }
}
"max=%d idx=%d\n", max_val, max_idx;
