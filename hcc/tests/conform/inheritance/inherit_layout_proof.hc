// inherit_layout_proof.hc — prove layout: base field at offset 0

#include <stdio.hh>
class Base { I64 first; };
class Derived : Base { I64 second; };
Derived d; d.first = 111; d.second = 222;
// Cast derived to I64* and read first slot = first field
I64 *raw = (I64 *)&d;
"%d %d\n", raw[0], raw[1];
