// offset_keyword.hc — offset() keyword returns byte offset of a field
class Hdr { I64 id; I64 len; I64 flags; };
"%d\n", offset(Hdr.id);
"%d\n", offset(Hdr.len);
"%d\n", offset(Hdr.flags);
