// djb2 string hash iterated, carrying the running hash across rounds.
U8 *s = "the quick brown fox jumps over the lazy dog 0123456789";
I64 rep, h = 5381;
for (rep = 0; rep < 200000; rep++) {
  I64 i = 0;
  while (s[i]) { h = ((h << 5) + h + s[i]) & 0x7FFFFFFF; i++; }
}
"%d\n", h;
