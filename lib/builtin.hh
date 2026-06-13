#ifndef _BUILTIN_HH
#define _BUILTIN_HH
// builtin.hh — the implicit prelude.
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

// Predefined **target macros** (compiler-injected, not defined here — like the
// command line below). The compiler seeds these into the preprocessor for the target
// being compiled for, mirroring C, so platform-specific code can be gated with
// `#ifdef`. `__HCC__` is always defined (the compiler marker, like C's
// `__GNUC__`); each is `1`. By target:
//
//   aarch64-apple-darwin : __APPLE__ __MACH__ __unix__ __aarch64__ __HCC__
//   x86_64-unknown-linux : __linux__ __unix__ __x86_64__ __HCC__
//   aarch64-unknown-linux: __linux__ __unix__ __aarch64__ __HCC__
//   x86_64-pc-windows    : _WIN32 _WIN64 __x86_64__ __HCC__
//
// The interpreter and `#exe` compile-time blocks run on the host, so they see the
// host's macros; a cross-compiled binary sees the target's. The Windows-only header
// `<windows.hc>` (C analog `<windows.h>`) is gated on `_WIN32`. Example:
//
//   #ifdef _WIN32  /* Windows-only */  #else  /* POSIX */  #endif

// The heap allocator `MAlloc`/`Free` is NOT in this prelude — it lives in `<stdlib.hc>`
// alongside `CAlloc`/`ReAlloc`/`HeapExtend`/`MSize`, mirroring C's `<stdlib.h>`. Use it
// with `#include <stdlib.hc>` (the `mem*` family is in `<string.hc>`). `MAlloc`/`Free`
// are irreducible compiler primitives, not HolyC: the compiler is their implementation
// (an `mmap` bump allocator freestanding, libc `malloc`/`free` hosted).

// `argc`/`argv` are dual-purpose implicit names, resolved by scope (no `#include`):
//
//   * At **top-level scope** (outside any function) they are the **command line**,
//     captured at the entry. `argv[i]` is a NUL-terminated string and `argv[0]` is the
//     program name, so `argc >= 1`:  I64 argc;  U8 **argv;  Command-line handling lives at
//     the top level, not in a function.
//
//   * Inside a `...` function they are the **variadic arguments**. `argc` is the count;
//     `argv[i]` is the i-th raw 8-byte slot — index it directly for an I64, or pun the
//     slot's address for another type, e.g. `*(F64 *)&argv[i]` or `*(U8 **)&argv[i]`:
//     I64 argc;  I64 *argv;
//
//   * Inside a non-variadic function they are neither — referencing them there is an
//     "undeclared identifier" error.
//
// Sema seeds both and the backends/interpreter lower them: they are not real declarations
// here. The command-line capture cost is paid only when `argc`/`argv` are used at top
// level.
//
// The environment is the implicit global `U8 **envp`, a NULL-terminated array of
// "KEY=VALUE" strings, captured at the entry and in scope everywhere (it is never
// shadowed). It is the low-level primitive: for a lookup by name use `Getenv("NAME")`
// from `<stdlib.hc>` (pure HolyC over `envp`); walk `envp` directly only to iterate the
// whole environment:
//
//   extern U8   **envp;   // I64 i = 0; while (envp[i]) { /* "%s\n", envp[i]; */ i++; }
//
// The capture cost is paid only when `envp` is referenced. On Windows it is NULL for
// now, because the OS environment is a different shape there.

// The current task/thread context, exposed as the implicit global `CTask *Fs`
// (sema-injected, like the command line above). It holds the exception state used by
// `try`/`catch`/`throw`: inside a `catch` block the thrown value is `Fs->except_ch`,
// and `Fs->catch_except` is 1 while an exception is being handled. It also holds the
// implicit `errno` (`err`): a failing OS primitive stores its error code there (the
// compiler emits the store), read via `<errno.hc>`'s `errno` macro or `Errno()`. `Fs`
// is per-thread (thread-local), so concurrent threads have independent exception and
// errno state. The `self`/`exc_top` fields are compiler-managed; user code reads
// `except_ch` and `err`.
public class CTask {
  U8  *self;         // self-pointer (the TLS slot stores &CTask)
  I64  except_ch;    // the value passed to `throw`, readable inside `catch`
  I64  catch_except; // 1 while handling an exception, else 0
  U8  *exc_top;      // top of this thread's try/catch handler-frame chain
  I64  err;          // errno of the last failed OS primitive (see <errno.hc>)
};

#endif
