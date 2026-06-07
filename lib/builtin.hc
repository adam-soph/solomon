#ifndef _BUILTIN_HC
#define _BUILTIN_HC
// builtin.hc — the implicit prelude.
//
// The compiler streams this ahead of every program. It is auto-included by
// `parse_with`, so no `#include` is needed. It gives every program the handful of
// true builtins: the predefined constants, and the primitives that can't be ordinary
// library functions because they read hidden globals or need ABI support. The
// backends and interpreter still lower these specially; the prototypes below only
// give sema their signatures, exactly like the printf family in <stdio.hc>.

// Predefined constants.
#define NULL  0
#define TRUE  1
#define FALSE 0

// The heap allocator. `MAlloc`/`Free` are irreducible compiler primitives, not HolyC:
// the compiler is their implementation (an `mmap` bump allocator freestanding, libc
// `malloc`/`free` hosted). Like the command line below, they are ambient with no
// `#include`. `MAlloc(n)` returns `n` uninitialised bytes; `Free(p)` releases them (a
// no-op on the bump allocators). The advanced heap primitives `HeapExtend`/`MSize` and
// the `ReAlloc`/`CAlloc` helpers built on these live in `<stdlib.hc>`; the `mem*` family
// is in `<string.hc>`.
public U8 *MAlloc(I64 n);
public U0 Free(U8 *ptr);

// The command line is exposed as two implicit globals, captured at the entry and in
// scope everywhere with no `#include`. `ArgV[i]` is a NUL-terminated string; `ArgV[0]`
// is the program name, so `ArgC >= 1`. Sema seeds these and the backends/interpreter
// lower them: they are not real declarations here.
//
//   extern I64    ArgC;   // argument count
//   extern U8   **ArgV;   // the arguments
//
// The environment is the implicit global `U8 **EnvP`, a NULL-terminated array of
// "KEY=VALUE" strings. It is captured at the entry, like the command line, and is
// sema-injected (only documented here). It is the low-level primitive: for a lookup
// by name, use `Getenv("NAME")` from `<stdlib.hc>` (pure HolyC over `EnvP`). Walk `EnvP`
// directly only to iterate the whole environment:
//
//   extern U8   **EnvP;   // I64 i = 0; while (EnvP[i]) { /* "%s\n", EnvP[i]; */ i++; }
//
// The capture cost is paid only when `EnvP` is referenced. On Windows it is NULL for
// now, because the OS environment is a different shape there.
//
// Inside any `...` function the compiler injects two implicit locals naming the
// variadic arguments. These are distinct from the command line above, so both coexist
// there. `VargV[i]` is the i-th raw 8-byte slot: index it directly for an I64, or pun
// the slot's address for another type, e.g. `*(F64 *)&VargV[i]` or `*(U8 **)&VargV[i]`.
//
//   I64    VargC;         // number of variadic args passed
//   I64   *VargV;         // their raw 8-byte slots

// The current task/thread context, exposed as the implicit global `CTask *Fs`
// (sema-injected, like the command line above). It holds the exception state used by
// `try`/`catch`/`throw`: inside a `catch` block the thrown value is `Fs->except_ch`,
// and `Fs->catch_except` is 1 while an exception is being handled. `Fs` is per-thread
// (thread-local), so concurrent threads have independent exception state. The
// `self`/`exc_top` fields are compiler-managed; user code reads `except_ch`.
public class CTask {
  U8  *self;         // self-pointer (the TLS slot stores &CTask)
  I64  except_ch;    // the value passed to `throw`, readable inside `catch`
  I64  catch_except; // 1 while handling an exception, else 0
  U8  *exc_top;      // top of this thread's try/catch handler-frame chain
};

#endif
