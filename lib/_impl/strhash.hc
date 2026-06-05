#ifndef _IMPL_STRHASH_HC
#define _IMPL_STRHASH_HC
// _impl/strhash.hc — a PRIVATE standard-library module. It lives under the
// `_impl/` directory, so (Go-`internal/`-style) its symbols are visible only to
// files within `_impl/`'s parent subtree — the rest of the standard library — and
// are a compile error if referenced from a user program. The names need no special
// convention; the `_`-prefixed *directory* is what makes them private.

// djb2 string hash, reduced to a non-negative I64.
I64 Djb2(U8 *s)
{
  I64 h = 5381;
  I64 i = 0;
  while (s[i] != 0) { h = h * 33 + s[i]; i++; }
  return h & 0x7FFFFFFFFFFFFFFF;
}

#endif
