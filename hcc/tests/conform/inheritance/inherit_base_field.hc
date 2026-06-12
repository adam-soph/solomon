// inherit_base_field.hc — access base field through derived instance

#include <stdio.hh>
class Animal { I64 legs; };
class Dog : Animal { I64 fur; };
Dog d; d.legs = 4; d.fur = 1;
"%d %d\n", d.legs, d.fur;
