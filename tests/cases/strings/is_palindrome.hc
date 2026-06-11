// is_palindrome.hc — check if a string is a palindrome
U8 *s = "racecar";
I64 n = 0;
while (s[n] != 0) n++;
I64 ok = 1, i = 0;
while (i < n / 2) {
    if (s[i] != s[n - 1 - i]) { ok = 0; break; }
    i++;
}
"%d\n", ok;

U8 *s2 = "hello";
n = 0;
while (s2[n] != 0) n++;
ok = 1; i = 0;
while (i < n / 2) {
    if (s2[i] != s2[n - 1 - i]) { ok = 0; break; }
    i++;
}
"%d\n", ok;
