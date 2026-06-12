// print_computed_chars.hc — print characters computed from arithmetic

#include <stdio.hh>
I64 i = 0;
while (i < 5) {
    "%c", 'a' + i;
    i++;
}
"\n";
// digits
i = 0;
while (i < 10) {
    "%c", '0' + i;
    i++;
}
"\n";
