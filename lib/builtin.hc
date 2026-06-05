#ifndef _BUILTIN_HC
#define _BUILTIN_HC
// builtin.hc — the implicit prelude. The compiler streams this ahead of every
// program (it's auto-included by `parse_with`, so no `#include` is needed), giving
// every program the handful of true builtins: the predefined constants and the
// primitives that can't be ordinary library functions (they read hidden globals or
// need ABI support). The backends and interpreter still *lower* these specially —
// the prototypes below only give sema their signatures, exactly like the printf
// family in <fmt.hc>.

// Predefined constants.
#define NULL  0
#define TRUE  1
#define FALSE 0

// The heap allocator. `MAlloc`/`Free` are irreducible compiler primitives — the
// compiler *is* their implementation (an `mmap` bump allocator freestanding, libc
// `malloc`/`free` hosted), not HolyC — so, like the command line below, they are
// ambient with no `#include`. `MAlloc(n)` returns `n` uninitialised bytes; `Free(p)`
// releases them (a no-op on the bump allocators). The advanced heap primitives
// `HeapExtend`/`MSize` and the `mem*`/`ReAlloc` helpers built on these stay in
// <mem.hc>.
U8 *MAlloc(I64 n);
U0 Free(U8 *ptr);

// The command line is exposed as two implicit globals, captured at the entry and in
// scope everywhere with no `#include`. `ArgV[i]` is a NUL-terminated string; `ArgV[0]`
// is the program name, so `ArgC >= 1`. (Sema seeds these; the backends/interpreter
// lower them — they are not real declarations here.)
//
//   extern I64    ArgC;   // argument count
//   extern U8   **ArgV;   // the arguments
//
// The environment is the implicit global `U8 **EnvP` — a **NULL-terminated** array of
// "KEY=VALUE" strings (captured at the entry, like the command line; sema-injected,
// only documented here). It's the low-level primitive; for a lookup by name use
// `Getenv("NAME")` from `<os.hc>` (pure HolyC over `EnvP`). Walk `EnvP` directly only
// to iterate the whole environment:
//
//   extern U8   **EnvP;   // I64 i = 0; while (EnvP[i]) { /* "%s\n", EnvP[i]; */ i++; }
//
// (The capture cost is paid only when `EnvP` is referenced. On Windows it is NULL for
// now — the OS environment is a different shape there.)
//
// Inside any `...` function the compiler injects two implicit locals naming the
// variadic arguments (distinct from the command line above, so both coexist there).
// `VargV[i]` is the i-th raw 8-byte slot — index it directly for an I64, or pun the
// slot's address for another type, e.g. `*(F64 *)&VargV[i]` or `*(U8 **)&VargV[i]`.
//
//   I64    VargC;         // number of variadic args passed
//   I64   *VargV;         // their raw 8-byte slots

#endif
