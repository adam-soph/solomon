// inherit_modify_derived_via_base.hc — write base fields via base ptr, read from derived
class Vec { I64 x; I64 y; };
class VecZ : Vec { I64 z; };
VecZ v; v.x = 0; v.y = 0; v.z = 9;
Vec *vp = (Vec *)&v;
vp->x = 3; vp->y = 4;
"%d %d %d\n", v.x, v.y, v.z;
