// class_mixed_narrow.hc — class mixing U8, U16, I32, I64
class Hdr { U8 ver; U8 kind; U16 flags; I32 len; I64 id; };
Hdr h;
h.ver = 1; h.kind = 2; h.flags = 0x8000; h.len = 100; h.id = 999;
"%d %d %d %d %d\n", h.ver, h.kind, h.flags, h.len, h.id;
