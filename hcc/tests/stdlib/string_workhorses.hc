
#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  U8 buf[32]; StrCpy(buf, "ab"); StrNCat(buf, "cdef", 2);   // strncat
  "%s\n", buf;
  "%s\n", StrPBrk("hello, world", " ,");                     // strpbrk
  "%d %d %d\n", StrCaseCmp("Hello", "hello"), StrCaseCmp("abc", "abd"),
                StrNCaseCmp("ABCxx", "abcyy", 3);            // strcasecmp/strncasecmp
  U8 *d = StrDup("dup"); "%s\n", d; Free(d);                 // strdup
  U8 *nd = StrNDup("truncated", 5); "%s\n", nd; Free(nd);    // strndup
  U8 s1[32]; StrCpy(s1, "a,bb,,ccc");                        // strtok (empty fields skipped)
  U8 *t = StrTok(s1, ",");
  while (t) { "%s.", t; t = StrTok(NULL, ","); }
  "\n";
  U8 s2[32]; StrCpy(s2, "one  two");                         // strtok_r
  U8 *sv; U8 *r = StrTokR(s2, " ", &sv);
  while (r) { "%s.", r; r = StrTokR(NULL, " ", &sv); }
  "\n";
  U8 mb[16]; U8 *q = MemCCpy(mb, "key=v", '=', 16); *q = 0;  // memccpy
  "%s\n", mb;
}
Main;
