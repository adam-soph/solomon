#ifndef _STDLIB_HH
#define _STDLIB_HH
// stdlib.hh — C `<stdlib.h>`: general utilities. Memory allocation — the base
// `MAlloc`/`Free` allocator pair, `CAlloc`/`ReAlloc`, and the advanced heap primitives
// `HeapExtend`/`MSize` — numeric conversion (the `atoi`/`atof` family), integer division
// (`Div`, C `div`/`ldiv`), pseudo-random numbers, sorting/searching (`qsort`/`bsearch`),
// process control (`Exit`/`Abort`/`AtExit`/`System`), and the environment
// (`Getenv`/`SetEnv`/`UnsetEnv`/`PutEnv`). Include with `#include <stdlib.hh>`.
//
// The correctly-rounded `atof` (`StrToF64`) works over the private `Bn` big integer
// defined here; `F64ToStr` (its shortest-round-trip inverse) reuses the float formatter
// `FmtFloat` from `<stdio.hc>`, so this includes it (a one-way dependency — `<stdio.hc>`
// does not depend back on this file, so a plain printing program stays lean).


// The base heap allocator (C `malloc`/`free`), the foundation `CAlloc`/`ReAlloc`/the
// containers build on. Irreducible compiler primitives, not HolyC: the compiler is
// their implementation (an `mmap` bump allocator freestanding, libc `malloc`/`free`
// hosted). `MAlloc(n)` returns `n` uninitialised bytes; `Free(p)` releases them (a no-op
// on the bump allocators).
#include <heap.hh>   // the base allocator MAlloc/Free (re-exported)

// =============================================================================
// Memory allocation: `CAlloc`/`ReAlloc` (C `calloc`/`realloc`), plus the advanced heap
// primitives `HeapExtend`/`MSize`. `MAlloc`/`Free` are declared in `<heap.hh>` (included
// above, and re-exported here).
// `HeapExtend`/`MSize` are intrinsics (the compiler is their implementation): an `mmap`
// bump allocator freestanding, or libc hosted. `HeapExtend(ptr,old,new)` grows the last
// block in place or returns NULL; `MSize(ptr)` returns the block's requested size.
// =============================================================================

public U8 *HeapExtend(U8 *ptr, I64 old, I64 newsz);
public I64 MSize(U8 *ptr);

// Allocate `n` zero-filled bytes (HolyC `CAlloc`, C `calloc`). `MAlloc` returns
// uninitialised memory, so this zeroes it explicitly.
public U8 *CAlloc(I64 n);

// Resize the block at `p` (originally `oldsz` bytes) to `newsz`, preserving the first
// min(oldsz, newsz) bytes. Returns the block, which may have moved. A bump allocator
// extends in place via `HeapExtend`, with no copy, when `p` is its last block.
// Otherwise it allocates a new block, copies, and frees the old one. `p == NULL` behaves
// like `MAlloc(newsz)`.
public U8 *ReAlloc(U8 *p, I64 oldsz, I64 newsz);

// =============================================================================
// Sorting & searching: `Sort`/`BSearch` (C `qsort`/`bsearch`), generic over the element
// type — typed throughout, with no element-size bookkeeping. The order is a
// caller-supplied comparator `I64 (*cmp)(T *a, T *b)` returning <0/0/>0. `base` must be a
// heap buffer (`MAlloc`'d or a `Vec`'s data): the interpreter byte-addresses heap blocks
// but not a cell-backed stack array. The `<vec.hc>` wrappers `VecSort`/`VecBSearch` are
// the usual entry points. The sort is a median-of-three quicksort with an insertion-sort
// cutoff; it is not stable; typical cost `O(n log n)`.
// =============================================================================

#define SORT_CUTOFF 16   // ranges this small are insertion-sorted

// Stock scalar element comparators. Each receives pointers to two elements. (`CmpStr`,
// for a `U8 *` string-pointer element, lives in `<string.hc>` next to `StrCmp`.)
public I64 CmpI64(I64 *a, I64 *b);
public I64 CmpU64(U64 *a, U64 *b);
public I64 CmpF64(F64 *a, F64 *b);

// `Sort`/`BSearch` and their helpers are templates the parser must register *before* any
// use site (generics are define-before-use), so they cannot be deferred to the end like
// an ordinary `.hc` implementation. They live in `<stdlib.hc>`, included at the foot of
// this header — the C++ template-header idiom — so they are parsed eagerly with these
// declarations. The prototypes are listed here for the reader; the bodies are in the
// implementation file:
//
//   U0  SortSwap     <type T>(T *a, T *b);
//   U0  SortInsertion<type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *));
//   U0  SortQuick    <type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *));
//   U0  Sort         <type T>(T *base, I64 n, I64 (*cmp)(T *, T *));
//   T  *BSearch      <type T>(T *key, T *base, I64 n, I64 (*cmp)(T *, T *));

// C `div`/`ldiv`: the quotient and remainder together, returned as a tuple (both are I64
// here, so one function serves both). Truncates toward zero like C — `Div(7,2)` is `(3,1)`,
// `Div(-7,2)` is `(-3,-1)`. Unpack with `q, r := Div(a, b);`.
public (I64, I64) Div(I64 num, I64 den);

// `strtol`: parse an integer in `base`, C-style. Skips leading whitespace and an optional
// sign. `base` 0 auto-detects ("0x"/"0X" -> 16, a leading "0" -> 8, else 10); base 16 also
// accepts an optional "0x"/"0X" prefix. Parsing stops at the first character that is not a
// digit of the base. If `end` is non-NULL, `*end` is set just past the last digit
// consumed, or to `s` (the original start) when no digits were found — so a caller can
// detect failure and resume scanning. Wraps on overflow.
public I64 StrToI64Base(U8 *s, I64 base, U8 **endp);

// Parse a base-10 integer, like atoll. Skips leading whitespace and an optional sign,
// then reads digits. Wraps on overflow. (`StrToI64Base` adds an arbitrary base + endptr.)
public I64 StrToI64(U8 *s);

// `strtoul`: the unsigned sibling of `StrToI64Base` — identical parsing (base, prefix,
// sign, endptr), but the result is interpreted unsigned, so a leading `-` wraps ("-1" in
// base 10 is U64 max) and values up to 2^64-1 read back correctly. Print it with `%u`.
public U64 StrToU64Base(U8 *s, I64 base, U8 **endp);

// Format n as decimal into buf (matching "%d") and return buf. Digits are extracted
// in the non-positive domain, so I64 min doesn't overflow on negation.
public U8 *I64ToStr(I64 n, U8 *buf);

// `StrToF64End` is a correctly-rounded decimal -> double parser with an endptr — the
// freestanding `strtod`. It returns the IEEE-754 double nearest the decimal value, ties to
// even, so for the normal range it is bit-for-bit a good libc `strtod`. Pure HolyC, so it
// is the *same* on the interpreter and every backend (the freestanding targets have no
// libc). If `endp` is non-NULL, `*endp` is set just past the consumed number, or to `str`
// (the original start) when no digits were found.
//
// Grammar (like `atof`): optional leading ASCII whitespace, an optional sign, then
// `digits[.digits]` and an optional `e`/`E` exponent. Parsing stops at the first
// character that doesn't fit. With no digits the result is 0.0.
//
// Two paths: a fast Clinger path (<=15 exact digits, power of ten in [-22, 22]: one
// correctly-rounded multiply or divide) and an exact path over the `Bn` big integer
// (build num/den, normalise into [2^52, 2^53) by powers of two, extract the 53-bit
// mantissa by binary long division, round half-to-even). Significands past ~40 digits
// are truncated (sub-ULP); subnormals are best-effort; the whole normal range is exact.
public F64 StrToF64End(U8 *str, U8 **endp);

// `atof`: parse a decimal double, ignoring where it stops. (`StrToF64End` adds an endptr.)
public F64 StrToF64(U8 *str);

// Format `v` as the **shortest** decimal string that `StrToF64` parses back to exactly
// `v` — the round-trip inverse of `StrToF64`. It tries increasing `%g` precision; 17
// significant digits always round-trips a `F64`, so the loop terminates. Reuses the
// correctly-rounded float formatter (`FmtFloat`, from `<stdio.hc>`) and `StrToF64` to
// verify. `buf` should be at least 32 bytes. Non-finite values (`inf`/`nan`) don't
// round-trip through `StrToF64`, so they fall through to the 17-digit form. Returns `buf`.
public U8 *F64ToStr(F64 v, U8 *buf);

// Set the generator's seed; the next `RandU64` continues the stream from here.
public U0 SeedRand(U64 seed);

// The next pseudo-random 64-bit value (splitmix64).
public U64 RandU64();

// =============================================================================
// Process control & environment
// =============================================================================

// Terminate the process with exit status `code` (its low 8 bits, per the OS
// convention), first running the `AtExit` handlers (the compiler injects the run).
// Does not return. An intrinsic: lowered to `exit_group`/`exit`/`ExitProcess` per
// target.
public U0 Exit(I64 code);

// C `_Exit`: terminate immediately, WITHOUT running the `AtExit` handlers. The same
// underlying primitive as `Exit`; only the spelling differs, and the compiler keys
// the handler run on the `Exit` spelling.
public U0 ExitRaw(I64 code);

// --- atexit -------------------------------------------------------------------

// An exit handler: no arguments, no result.
typedef U0 (*)() AtExitFn;

#define ATEXIT_MAX 32   // C guarantees at least 32 registrations

// C `atexit`: register `fn` to run at normal termination — an explicit `Exit` or the
// top level ending — in reverse (LIFO) order. Returns 0, or -ENOMEM once the table
// (ATEXIT_MAX) is full. `ExitRaw`/`Abort` and an uncaught `throw` skip the handlers,
// like C's `_Exit`/`abort`/abnormal termination.
public I64 AtExit(AtExitFn fn);

// C `abort`: write "Aborted\n" to stderr and terminate with status 134 (128+SIGABRT,
// what a shell reports for a real abort) without running the `AtExit` handlers.
// (There is no signal machinery to raise SIGABRT itself.)
public U0 Abort();

// C `system`: run `cmd` through the shell (`/bin/sh -c cmd`; `cmd /C` on a Windows
// host's interpreter) and wait for it. Returns the child's exit code (0–255), or a
// negative value when the spawn fails or the child dies abnormally (also recorded in
// `errno` where the OS reports one). An intrinsic: the interpreter uses Rust
// `std::process`, Darwin libc `system`, freestanding Linux a raw fork/execve/wait4 —
// where the child gets an empty environment — and the Windows native target rejects
// it at compile time for now. Impure, like the other process primitives.
public I64 System(U8 *cmd);

// Look up environment variable `name`. Returns a pointer to its value (the bytes after
// `name=` in the matching entry), or NULL if it is unset. `SetEnv`/`UnsetEnv` overrides
// take precedence over the process environment (`envp`, the implicit `environ` array,
// sema-injected). The result is read-only: do not free or modify it, and a later
// `SetEnv`/`UnsetEnv` of the same name invalidates it.
public U8 *Getenv(U8 *name);

// Set environment variable `name` to `val` (C `setenv`, always overwriting). The pair
// is heap-copied into the override table consulted by `Getenv`; the process
// environment itself is untouched, so the override is visible to this program (and
// `#exe`), not to `System` children. Returns 0, or -EINVAL for an invalid name
// (empty, or containing '='). Like C, the values persist for the rest of the run;
// they are never freed.
public I64 SetEnv(U8 *name, U8 *val);

// Remove `name` from the environment (C `unsetenv`): a subsequent `Getenv` returns
// NULL even if the process environment has it. Returns 0, or -EINVAL for an invalid
// name.
public I64 UnsetEnv(U8 *name);

// C `putenv`: install a whole "name=value" string. Unlike C the string is copied (the
// caller keeps ownership of `str`); a `str` with no '=' unsets the name, matching the
// common (glibc) behavior. Returns 0, or -EINVAL for an empty/'='-leading string.
public I64 PutEnv(U8 *str);

#include <stdlib.hc>

#endif
