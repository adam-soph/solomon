#include <stdio.hh>
#include <stdlib.hh>
#include <string.hh>
U0 Main() {
  U8 s[32]; StrCpy(s, "a,,b,");      // empty fields kept (unlike StrTok)
  U8 *p = s, *t;
  while ((t = StrSep(&p, ","))) "[%s]", t;
  "\n";
  U8 s2[32]; StrCpy(s2, ",x"); p = s2; // leading delimiter -> empty first field
  while ((t = StrSep(&p, ","))) "[%s]", t;
  "\n";
  U8 s3[32]; StrCpy(s3, "name=value"); p = s3;
  U8 *k = StrSep(&p, "="), *v = StrSep(&p, "=");
  "%s=%s null=%d\n", k, v, p == NULL;
}
Main;
