// stdlib.hc — a tour of the core-library built-ins: building strings on the
// heap (MAlloc/StrCpy/StrCat/StrLen), in-place character case (ToUpper),
// comparison (StrCmp/MemCmp), raw memory (MemSet/MemCpy/Free), and math
// (Sqrt/Pow/Floor/Ceil/Round/Sin/Cos). Output is kept integer-clean so it is
// identical under both the interpreter and the native backend.

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

  // Math, cast to integers so the output is deterministic across backends.
  "sqrt=%d pow=%d\n", (I64)Sqrt(144.0), (I64)Pow(2.0, 10.0);
  "floor=%d ceil=%d round=%d\n", (I64)Floor(3.9), (I64)Ceil(3.1), (I64)Round(2.5);
  "trig=%d\n", (I64)(Sin(0.0) + Cos(0.0)); // 0 + 1
}

Main;
