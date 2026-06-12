// Class with an array field; initialize with nested braces.

#include <stdio.hh>
class Row { I64 data[3]; };
Row r = {{7, 8, 9}};
"%d %d %d\n", r.data[0], r.data[1], r.data[2];
