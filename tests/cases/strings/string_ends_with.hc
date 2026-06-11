// string_ends_with.hc — check if string ends with a suffix
U8 *s = "foobar";
U8 *suf = "bar";
// compute lengths
I64 sn = 0, sufn = 0;
while (s[sn] != 0) sn++;
while (suf[sufn] != 0) sufn++;
I64 ok = 0;
if (sn >= sufn) {
    ok = 1;
    I64 i = 0;
    while (i < sufn) {
        if (s[sn - sufn + i] != suf[i]) { ok = 0; break; }
        i++;
    }
}
"%d\n", ok;

U8 *suf2 = "foo";
sufn = 0;
while (suf2[sufn] != 0) sufn++;
ok = 0;
if (sn >= sufn) {
    ok = 1;
    I64 i = 0;
    while (i < sufn) {
        if (s[sn - sufn + i] != suf2[i]) { ok = 0; break; }
        i++;
    }
}
"%d\n", ok;
