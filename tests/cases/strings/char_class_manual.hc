// char_class_manual.hc — test digit/alpha by hand without ctype
I64 IsD(U8 c) { return c >= '0' && c <= '9'; }
I64 IsA(U8 c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'); }
"%d %d\n", IsD('5'), IsD('x');
"%d %d\n", IsA('z'), IsA('9');
