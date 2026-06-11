// macro_computed_val.hc — macros computing values printed at runtime
#define KILO 1000
#define MEGA (KILO * KILO)
#define GIGA (MEGA * KILO)
I64 k = KILO, m = MEGA, g = GIGA;
"%d %d %d\n", k, m, g;
