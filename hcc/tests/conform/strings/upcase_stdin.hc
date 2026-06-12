//@ stdin: hello world
//@ stdin: second line
// Read each stdin line and echo it upper-cased.

#include <stdio.hh>
#include <string.hh>
#include <unistd.hh>
#include <unistd.hh>   // STDOUT/STDIN/STDERR

U8 *line;
while ((line = ReadLine(STDIN))) {
  StrToUpper(line);
  "%s\n", line;
  Free(line);
}
