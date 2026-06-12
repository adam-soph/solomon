// Multiple cases with the same body via fall-through.

#include <math.hh>
#include <stdio.hh>
I64 x = 'E';
switch (x) {
  case 'A':
  case 'E':
  case 'I':
  case 'O':
  case 'U':
    "vowel\n";
    break;
  default:
    "consonant\n";
}
