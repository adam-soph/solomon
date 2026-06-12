// string_nul_byte.hc — a NUL byte in a string literal truncates %s output
// We print characters individually up to a known boundary instead.

#include <stdio.hh>
U8 *s = "ab";
"%c%c\n", s[0], s[1];
"%d\n", s[2];
