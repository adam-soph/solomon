// switch on a U8 / character value.

#include <stdio.hh>
U8 c = 'b';
switch (c) {
  case 'a': "alpha\n"; break;
  case 'b': "bravo\n"; break;
  case 'c': "charlie\n"; break;
  default: "other\n";
}
