// string_contains_substr.hc — check if a string contains a substring

#include <stdio.hh>
U8 *hay = "the quick brown fox";
U8 *needle = "quick";
I64 found = 0, i = 0;
while (hay[i] != 0 && !found) {
    I64 j = 0, ok = 1;
    while (needle[j] != 0) {
        if (hay[i + j] == 0 || hay[i + j] != needle[j]) { ok = 0; break; }
        j++;
    }
    if (ok) found = 1;
    i++;
}
"%d\n", found;

U8 *needle2 = "lazy";
found = 0; i = 0;
while (hay[i] != 0 && !found) {
    I64 j = 0, ok = 1;
    while (needle2[j] != 0) {
        if (hay[i + j] == 0 || hay[i + j] != needle2[j]) { ok = 0; break; }
        j++;
    }
    if (ok) found = 1;
    i++;
}
"%d\n", found;
