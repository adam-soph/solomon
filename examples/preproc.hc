// preproc.hc — macros, conditionals, and a forward type reference (hoisting).
#define MAX_LEN 16
#define SQUARE(x) ((x) * (x))
#define DEBUG

I64 buffer[MAX_LEN];

I64 Area(I64 side)
{
  return SQUARE(side);
}

// `Thing` is used here but defined further down — type hoisting makes this a
// declaration rather than a parse error.
U0 Use()
{
  Thing t;
  t.id = SQUARE(3);
#ifdef DEBUG
  t.id = MAX_LEN;
#else
  t.id = 0;
#endif
}

class Thing
{
  I64 id;
};
