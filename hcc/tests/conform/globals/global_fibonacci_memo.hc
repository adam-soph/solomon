// Global memo table for Fibonacci (memoized via global array).

#include <stdio.hh>
#define MAX_FIB 10
I64 g_memo[MAX_FIB];
Bool g_has[MAX_FIB];

I64 Fib(I64 n) {
  if (n < 2) return n;
  if (g_has[n]) return g_memo[n];
  I64 v = Fib(n-1) + Fib(n-2);
  g_memo[n] = v;
  g_has[n] = TRUE;
  return v;
}

I64 i;
for (i = 0; i < MAX_FIB; i++) "%d ", Fib(i);
"\n";
