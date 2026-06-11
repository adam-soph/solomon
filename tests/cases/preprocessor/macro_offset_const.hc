// macro_offset_const.hc — constant macros used as bit/byte offsets
#define BIT0 1
#define BIT1 2
#define BIT2 4
#define BIT3 8
I64 flags = BIT0 | BIT2;
"%d\n", flags & BIT0;
"%d\n", flags & BIT1;
"%d\n", flags & BIT2;
"%d\n", (flags & BIT0) && (flags & BIT2);
