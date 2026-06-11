// string_compare_hand.hc — hand-rolled string comparison
U8 *a = "apple";
U8 *b = "banana";
U8 *c = "apple";
I64 i = 0;
// compare a vs c (equal)
I64 eq = 1;
while (a[i] != 0 || c[i] != 0) {
    if (a[i] != c[i]) { eq = 0; break; }
    i++;
}
"%d\n", eq;

// compare a vs b (not equal)
i = 0; eq = 1;
while (a[i] != 0 || b[i] != 0) {
    if (a[i] != b[i]) { eq = 0; break; }
    i++;
}
"%d\n", eq;
