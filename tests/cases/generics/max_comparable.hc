// max_comparable.hc — Max<comparable T> with I64 and F64
T Max<comparable T>(T a, T b) { return a > b ? a : b; }
T Min<comparable T>(T a, T b) { return a < b ? a : b; }
"%d\n", Max(3, 9);
"%d\n", Max(-5, -1);
"%.1f\n", Max(2.5, 1.5);
"%.1f\n", Min(3.0, 7.0);
"%d\n", Min(100, 50);
