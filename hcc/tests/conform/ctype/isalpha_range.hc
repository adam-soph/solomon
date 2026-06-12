
// uppercase letters
#include <ctype.hh>
#include <stdio.hh>
"%d %d %d\n", IsAlpha('A'), IsAlpha('M'), IsAlpha('Z');
// lowercase letters
"%d %d %d\n", IsAlpha('a'), IsAlpha('m'), IsAlpha('z');
// non-alpha
"%d %d %d\n", IsAlpha('0'), IsAlpha(' '), IsAlpha('!');
