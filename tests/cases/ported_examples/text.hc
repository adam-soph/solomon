// text.hc — simple text processing: count words by scanning, search for
// substrings with StrFind (which returns a pointer into the haystack), and
// uppercase a string in place. Output is integer/string only, so it is identical
// under the interpreter and the native backend.

#include <string.hc>  // StrLen/StrCpy/StrFind
#include <ctype.hc>   // ToUpper

I64 WordCount(U8 *s) {
  I64 words = 0;
  I64 in_word = 0;
  I64 i = 0;
  while (s[i] != 0) {
    if (s[i] == ' ') {
      in_word = 0;
    } else if (!in_word) {
      in_word = 1;
      words++;
    }
    i++;
  }
  return words;
}

U0 Main() {
  U8 *text = MAlloc(64);
  StrCpy(text, "the quick brown fox");
  "len=%d words=%d\n", StrLen(text), WordCount(text);

  // StrFind(haystack, needle) returns a pointer to the match (or NULL).
  "has_quick=%d has_slow=%d\n", StrFind(text, "quick") != NULL, StrFind(text, "slow") != NULL;
  U8 *fox = StrFind(text, "fox");
  "fox_at=%d\n", fox - text;

  // Uppercase in place.
  I64 i = 0;
  while (text[i] != 0) {
    text[i] = ToUpper(text[i]);
    i++;
  }
  "%s\n", text;

  Free(text);
}

Main;
