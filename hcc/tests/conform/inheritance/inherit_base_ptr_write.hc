// inherit_base_ptr_write.hc — write through base pointer, read through derived

#include <stdio.hh>
class Base { I64 id; };
class Sub : Base { I64 val; };
Sub s; s.id = 0; s.val = 5;
Base *bp = (Base *)&s;
bp->id = 99;
"%d %d\n", s.id, s.val;
