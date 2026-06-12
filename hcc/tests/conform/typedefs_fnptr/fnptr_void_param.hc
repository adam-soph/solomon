// Function pointer to a void function.

#include <stdio.hh>
#include <stdlib.hh>
U0 SayHi(I64 x) { "hi %d\n", x; }
U0 SayBye(I64 x) { "bye %d\n", x; }

U0 (*greet)(I64) = &SayHi;
greet(1);
greet = &SayBye;
greet(2);
