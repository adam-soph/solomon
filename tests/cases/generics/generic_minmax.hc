// generic_minmax.hc — Min/Max with I64, F64, and U8 (pointer comparable)
T Max<comparable T>(T a, T b) { return a > b ? a : b; }
T Min<comparable T>(T a, T b) { return a < b ? a : b; }
T Clamp<comparable T>(T x, T lo, T hi) { return Min(Max(x, lo), hi); }

"%d\n", Clamp(5, 1, 10);
"%d\n", Clamp(-3, 1, 10);
"%d\n", Clamp(15, 1, 10);
"%.1f\n", Clamp(2.5, 1.0, 5.0);
"%.1f\n", Clamp(0.5, 1.0, 5.0);
