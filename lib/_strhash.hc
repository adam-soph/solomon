#ifndef _STRHASH_HC
#define _STRHASH_HC
// _strhash.hc — a PRIVATE standard-library module. Its `_`-prefixed *filename*
// (Go-`internal/`-style, generalized to files) makes its symbols visible only to
// files within its own directory's subtree — the rest of the standard library — and
// a compile error if referenced from a user program. The names need no special
// convention; the `_`-prefixed file is what makes them private.

// djb2 string hash, reduced to a non-negative I64.
I64 Djb2(U8 *s)
{
  I64 h = 5381;
  I64 i = 0;
  while (s[i] != 0) { h = h * 33 + s[i]; i++; }
  return h & 0x7FFFFFFFFFFFFFFF;
}

#endif
