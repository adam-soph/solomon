// count_alpha_digit.hc — count alpha vs digit chars by hand
U8 *s = "abc123def456";
I64 alpha = 0, digit = 0, i = 0;
while (s[i] != 0) {
    U8 c = s[i];
    if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) alpha++;
    else if (c >= '0' && c <= '9') digit++;
    i++;
}
"alpha=%d digit=%d\n", alpha, digit;
