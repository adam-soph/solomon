#include <float.hh>
#include <math.hh>
#include <stdio.hh>
#include <stdlib.hh>
U0 Main() {
  // lround: ties away from zero
  "%d %d %d %d %d\n", LRound(2.5), LRound(-2.5), LRound(2.4), LRound(2.6), LRound(-0.5);
  // lrint: ties to even
  "%d %d %d %d\n", LRint(2.5), LRint(3.5), LRint(-2.5), LRint(2.6);
  F64 nan = NaN(), inf = Inf(1), sub = F64_TRUE_MIN;
  "%d %d %d %d\n", IsFinite(1.0), IsFinite(nan), IsFinite(inf), IsFinite(sub);
  "%d %d %d %d %d\n", IsNormal(1.0), IsNormal(0.0), IsNormal(sub), IsNormal(inf), IsNormal(nan);
  // FpClassify -> FP_NAN/INFINITE/ZERO/SUBNORMAL/NORMAL = 0..4
  "%d %d %d %d %d\n", FpClassify(nan), FpClassify(inf), FpClassify(0.0),
                      FpClassify(sub), FpClassify(1.5);
}
Main;
