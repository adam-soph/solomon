// class_return_field.hc — function returns a field of a class argument
class Vec2 { I64 x; I64 y; };
I64 GetX(Vec2 v) { return v.x; }
I64 GetY(Vec2 v) { return v.y; }
Vec2 v; v.x = 8; v.y = 15;
"%d %d\n", GetX(v), GetY(v);
