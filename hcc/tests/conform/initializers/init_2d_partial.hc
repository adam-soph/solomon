// Partial nested brace: inner list shorter than the row size, rest zero.

#include <stdio.hh>
I64 m[3][3] = {{1,2},{3}};
"%d %d %d\n", m[0][0], m[0][1], m[0][2];
"%d %d %d\n", m[1][0], m[1][1], m[1][2];
"%d %d %d\n", m[2][0], m[2][1], m[2][2];
