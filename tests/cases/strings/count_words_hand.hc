// count_words_hand.hc — count whitespace-separated words by hand
U8 *s = "the quick brown fox";
I64 words = 0, i = 0, inword = 0;
while (s[i] != 0) {
    if (s[i] == ' ' || s[i] == '\t') {
        inword = 0;
    } else {
        if (!inword) { words++; inword = 1; }
    }
    i++;
}
"%d\n", words;
