// string_starts_with.hc — check if string starts with a prefix
U8 *s = "foobar";
U8 *pre = "foo";
I64 i = 0, ok = 1;
while (pre[i] != 0) {
    if (s[i] != pre[i]) { ok = 0; break; }
    i++;
}
"%d\n", ok;

U8 *pre2 = "bar";
i = 0; ok = 1;
while (pre2[i] != 0) {
    if (s[i] != pre2[i]) { ok = 0; break; }
    i++;
}
"%d\n", ok;
