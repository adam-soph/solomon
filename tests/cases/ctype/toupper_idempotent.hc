#include <ctype.hc>
// ToUpper of an already-uppercase char is the same char
"%d\n", ToUpper('A') == 'A';
"%d\n", ToUpper('Z') == 'Z';
// ToLower of a lowercase char
"%d\n", ToLower('a') == 'a';
