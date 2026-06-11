// f64_field.hc — class with an F64 field
class Circle { F64 radius; };
Circle c; c.radius = 2.5;
F64 area = 3.14159265358979 * c.radius * c.radius;
"%f\n", area;
