// Contrast local vs global brace initializer for the same class.
class Vec3 { I64 x; I64 y; I64 z; };
Vec3 g = {1, 2, 3};

Vec3 l = {4, 5, 6};
"%d %d %d\n", g.x, g.y, g.z;
"%d %d %d\n", l.x, l.y, l.z;
