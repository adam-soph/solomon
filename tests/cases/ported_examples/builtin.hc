// builtin.hc — a tour of the core-library built-ins: building strings on the
// heap (MAlloc/StrCpy/StrCat/StrLen), in-place character case (ToUpper),
// comparison (StrCmp/MemCmp), raw memory (MemSet/MemCpy/Free), and the
// exactly-reproducible math op Sqrt. Output is kept integer-clean so it is
// identical under the interpreter and both native backends. (Transcendental
// functions — Sin/Cos/Pow/… — are intentionally not core builtins: their value
// would be only "whatever the host libm does," with no portable solomon
// semantics, so they're left to a future HolyC standard library.)

#include <string.hc>  // StrLen/StrCmp/StrCpy/StrCat + MemCpy/MemSet/MemCmp
#include <ctype.hc>   // ToUpper
#include <math.hc>    // Sqrt

// Uppercase a NUL-terminated string in place; returns its new length.
I64 Upcase(U8 *s) {
  I64 i = 0;
  while (s[i] != 0) {
    s[i] = ToUpper(s[i]);
    i++;
  }
  return i;
}

U0 Main() {
  // Build "Hello, World!" from pieces on the heap.
  U8 *msg = MAlloc(32);
  StrCpy(msg, "Hello");
  StrCat(msg, ", ");
  StrCat(msg, "World!");
  "%s len=%d\n", msg, StrLen(msg);

  Upcase(msg);
  "%s\n", msg;

  // Ordering and a byte-wise compare.
  "cmp=%d memcmp=%d\n", StrCmp("abc", "abd"), MemCmp("abc", "abc", 3);

  // Fill, then overwrite the front of a small buffer.
  U8 *buf = MAlloc(8);
  MemSet(buf, '-', 5);
  buf[5] = 0;
  MemCpy(buf, "OK", 2);
  "%s\n", buf;

  Free(msg);
  Free(buf);

  // Exactly-reproducible math, cast to an integer so output is deterministic.
  "sqrt=%d\n", (I64)Sqrt(144.0);
}

Main;
