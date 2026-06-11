// sq_abs_macros.hc — SQ/ABS style utility macros
#define SQ(x) ((x)*(x))
#define ABS(x) ((x) < 0 ? -(x) : (x))
#define SIGN(x) ((x) > 0 ? 1 : (x) < 0 ? -1 : 0)
"%d\n", SQ(7);
"%d\n", ABS(-5);
"%d\n", ABS(3);
"%d\n", SIGN(-10);
"%d\n", SIGN(0);
"%d\n", SIGN(5);
