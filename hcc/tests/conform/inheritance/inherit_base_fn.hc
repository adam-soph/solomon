// inherit_base_fn.hc — function that takes Base * and reads base fields

#include <stdio.hh>
class Vehicle { I64 wheels; };
class Car : Vehicle { I64 doors; };
I64 Wheels(Vehicle *v) { return v->wheels; }
Car c; c.wheels = 4; c.doors = 2;
"%d\n", Wheels((Vehicle *)&c);
