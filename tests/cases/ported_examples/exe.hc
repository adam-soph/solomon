// exe.hc — `#exe { ... }` runs HolyC at **compile time** (through the interpreter) and
// splices whatever it prints back into the source. Used here to build a lookup table
// and to generate constant declarations at build time — TempleOS's compile-time
// code-execution directive.

#include <math.hc>

// A cosine table computed by the compiler — no runtime trig. The block runs at compile
// time and emits the comma-separated literals into this initializer.
F64 CosTab[8] = {
  #exe {
    #include <math.hc>
    I64 i;
    for (i = 0; i < 8; i++) "%.6f,", Cos(i * 6.28318530717958 / 8);
  }
};

// Generate `I64 pow2_0 .. pow2_4 = 1,2,4,8,16;` at compile time.
#exe {
  I64 i, v = 1;
  for (i = 0; i < 5; i++) { "I64 pow2_%d = %d;\n", i, v; v *= 2; }
}

U0 Main()
{
  "cos0=%.4f cos2=%.4f cos4=%.4f\n", CosTab[0], CosTab[2], CosTab[4];
  "pow2: %d %d %d %d %d\n", pow2_0, pow2_1, pow2_2, pow2_3, pow2_4;
}

Main;
