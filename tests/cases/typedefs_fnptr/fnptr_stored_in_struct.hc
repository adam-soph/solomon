// Array of structs, each with a function pointer field.
class Handler {
  I64 id;
  U0 (*run)(I64);
};

U0 H0(I64 x) { "handler0: %d\n", x; }
U0 H1(I64 x) { "handler1: %d\n", x; }
U0 H2(I64 x) { "handler2: %d\n", x; }

Handler hs[3];
hs[0].id = 0; hs[0].run = &H0;
hs[1].id = 1; hs[1].run = &H1;
hs[2].id = 2; hs[2].run = &H2;

I64 i;
for (i = 0; i < 3; i++)
  hs[i].run(hs[i].id * 10);
