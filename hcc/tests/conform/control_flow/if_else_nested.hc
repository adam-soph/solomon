// Nested if/else.

#include <stdio.hh>
I64 a = 5, b = 10;
if (a > 0) {
  if (b > 0) {
    "both positive\n";
  } else {
    "a pos b not\n";
  }
} else {
  if (b > 0) {
    "b pos a not\n";
  } else {
    "neither positive\n";
  }
}
