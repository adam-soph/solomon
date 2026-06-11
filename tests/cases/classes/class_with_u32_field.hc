// class_with_u32_field.hc — class with U32 field; check narrow truncation
class Hdr { U32 magic; U32 size; };
Hdr h;
h.magic = 0xCAFEBABE;
h.size  = 256;
"%d %d\n", h.magic, h.size;
