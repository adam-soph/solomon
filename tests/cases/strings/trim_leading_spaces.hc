// trim_leading_spaces.hc — skip leading spaces
U8 *s = "   hello";
I64 i = 0;
while (s[i] == ' ') i++;
"%s\n", s + i;
