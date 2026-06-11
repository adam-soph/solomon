// inherit_overwrite_base.hc — write to base fields through derived pointer
class Base { I64 x; I64 y; };
class Derived : Base { I64 z; };
Derived d; d.x = 0; d.y = 0; d.z = 0;
Derived *dp = &d;
dp->x = 5; dp->y = 10; dp->z = 15;
"%d %d %d\n", d.x, d.y, d.z;
