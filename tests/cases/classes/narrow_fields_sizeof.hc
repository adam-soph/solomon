// narrow_fields_sizeof.hc — packed narrow fields + sizeof
class Packed { U8 a; U8 b; U16 c; I32 d; };
Packed p;
p.a = 0xFF; p.b = 0x01; p.c = 0x1234; p.d = -7;
"%d %d %d %d\n", p.a, p.b, p.c, p.d;
"%d\n", sizeof(Packed);
