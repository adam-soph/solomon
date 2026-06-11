//@ stdin: hello world
//@ stdin: second line
// Read each stdin line and echo it upper-cased.
#include <stdio.hc>

U8 *line;
while ((line = ReadLine(STDIN))) {
  StrToUpper(line);
  "%s\n", line;
  Free(line);
}
