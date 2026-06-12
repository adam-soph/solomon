// inherit_two_derived.hc — two derived classes of the same base

#include <stdio.hh>
class Animal { I64 legs; };
class Dog : Animal { I64 bark; };
class Bird : Animal { I64 wings; };
Dog d; d.legs = 4; d.bark = 1;
Bird b; b.legs = 2; b.wings = 2;
"%d %d\n", d.legs, d.bark;
"%d %d\n", b.legs, b.wings;
