// Global Bool array used as a visited set.
#define N 6
Bool g_visited[N];

U0 Visit(I64 i) { g_visited[i] = TRUE; }

Visit(1); Visit(3); Visit(5);
I64 i;
for (i = 0; i < N; i++) "%d ", g_visited[i];
"\n";
