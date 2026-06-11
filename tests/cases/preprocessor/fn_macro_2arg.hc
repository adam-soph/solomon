// fn_macro_2arg.hc — 2-argument function macros
#define ADD(a, b) ((a) + (b))
#define MUL(a, b) ((a) * (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define MIN(a, b) ((a) < (b) ? (a) : (b))
"%d\n", ADD(3, 4);
"%d\n", MUL(5, 6);
"%d\n", MAX(10, 20);
"%d\n", MIN(10, 20);
