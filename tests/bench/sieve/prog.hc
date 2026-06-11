// Sieve of Eratosthenes — count primes below N (zero-initialized global array).
#define N 300000
U8 sieve[N];
I64 i, j, count = 0;
for (i = 2; i < N; i++) {
  if (!sieve[i]) { count++; for (j = i + i; j < N; j += i) sieve[j] = 1; }
}
"%d\n", count;
