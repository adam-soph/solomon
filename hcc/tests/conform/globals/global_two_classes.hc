// Two global classes cross-referencing each other's data.

#include <stdio.hh>
#include <stdlib.hh>
class Stats { I64 sum; I64 cnt; };
class Config { I64 limit; };

Stats g_stats;
Config g_cfg;

U0 Setup() { g_cfg.limit = 5; }

U0 Record(I64 v) {
  if (g_stats.cnt < g_cfg.limit) {
    g_stats.sum += v;
    g_stats.cnt++;
  }
}

Setup();
I64 i;
for (i = 1; i <= 7; i++) Record(i);
"sum=%d cnt=%d\n", g_stats.sum, g_stats.cnt;
