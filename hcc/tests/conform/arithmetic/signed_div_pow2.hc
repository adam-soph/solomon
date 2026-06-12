// Signed divide/modulo by a power of two with a *runtime* (non-constant) dividend, over
// negatives and INT_MIN/INT_MAX. This exercises the backend's round-toward-zero strength
// reduction (arm64/x86 `try_imm_div`). Constant/constant division is folded away in `simplify`,
// so the dividend must arrive through a call to reach the reduced code path.

#include <stdio.hh>
I64 D2(I64 x) { return x / 2; }
I64 D4(I64 x) { return x / 4; }
I64 D8(I64 x) { return x / 8; }
I64 M2(I64 x) { return x % 2; }
I64 M4(I64 x) { return x % 4; }
I64 M8(I64 x) { return x % 8; }

I64 vals[9];
vals[0] = 1 << 63;        // INT_MIN
vals[1] = -8;
vals[2] = -7;
vals[3] = -1;
vals[4] = 0;
vals[5] = 1;
vals[6] = 7;
vals[7] = 8;
vals[8] = (1 << 63) - 1;  // INT_MAX

I64 i;
for (i = 0; i < 9; i++) {
  I64 x = vals[i];
  "%d %d %d %d %d %d\n", D2(x), M2(x), D4(x), M4(x), D8(x), M8(x);
}
