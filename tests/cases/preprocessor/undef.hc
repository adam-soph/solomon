// undef.hc — #undef removes a macro definition
#define TEMP 100
I64 x = TEMP;
"%d\n", x;
#undef TEMP
#define TEMP 200
I64 y = TEMP;
"%d\n", y;
