#include <stdlib.hc>
// hex
"%d\n", StrToI64Base("0xff", 0, NULL);
// octal
"%d\n", StrToI64Base("010", 0, NULL);
// explicit base 2
"%d\n", StrToI64Base("1010", 2, NULL);
