// throw_unwind_loop.hc — throw unwinds out of a loop inside a called function
I64 FindFirst(I64 *arr, I64 n, I64 val) {
  I64 i;
  for (i = 0; i < n; i++) {
    if (arr[i] == val) throw(i);
  }
  return -1;
}
I64 data[5];
data[0]=10; data[1]=20; data[2]=30; data[3]=40; data[4]=50;
try {
  FindFirst(data, 5, 30);
  "not found\n";
} catch {
  "found at index %d\n", Fs->except_ch;
}
