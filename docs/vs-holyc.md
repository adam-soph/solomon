# solomon HolyC vs. original (TempleOS) HolyC

solomon reimplements the **HolyC language** as a from-scratch, cross-platform compiler
and interpreter. Terry Davis's original HolyC was inseparable from **TempleOS** — it ran
in ring-0, JIT/AOT-compiled against the live kernel, and its "standard library" *was* the
operating system. solomon instead produces ordinary hosted or freestanding programs
(macOS Mach-O, Linux/x86-64 + Linux/aarch64 static ELFs, a Windows PE) and reimplements
only the **portable, reducible** part of the library as a C-style stdlib.

So most differences fall into two big themes: solomon **adds** a real type system and
generics on the language side, and **drops** the entire OS-integrated runtime on the
library side. Buckets below: **shared**, **added** (solomon, not HolyC), **missing**
(HolyC, not solomon), and **different behavior**.

> Scope note: "original HolyC" here means the language plus the reducible parts of its
> library. The bulk of TempleOS's API (graphics, sound, tasks, the filesystem, DolDoc, the
> REPL) is the OS itself and is out of scope for a hosted reimplementation — those are
> listed under *missing* but were never going to be portable.

---

## Language — shared with HolyC

These work the same in both:

- **Types**: `U0` (void), `I8`/`U8`/`I16`/`U16`/`I32`/`U32`/`I64`/`U64`, `F64`, `Bool`.
  Default integer is `I64`. Only `F64` floats (no `F32`/`F32` in either).
- **`class` / `union`** (no `struct` keyword), `repr(C)` layout, anonymous unions promote
  their members, arrays decay to pointers, classes pass/assign by value.
- **A bare string statement prints itself** (`"hi\n";`), and the comma form is printf-style
  (`"x=%d\n", x;`).
- **A bare function name is a call**: `Main;`.
- **`switch`** with `start:`/`end:` sub-labels (prologue/epilogue), plus C `case`/`default`.
- **Chained range comparisons**: `a < b < c`, `0 <= i < n`.
- **Exceptions**: `try { } catch { }` / `throw expr;`, with the caught value read off the
  implicit `Fs->except_ch` (`Fs` being the sema-injected `CTask *`).
- **`#exe { … }`** — run HolyC at compile time and splice its stdout into the source.
- **Default arguments** (`U0 F(I64 x = 5)`), **`goto`** + labels, `sizeof`, `offset`,
  the preprocessor (`#define`/`#ifdef`/`#include`).
- PascalCase library names where solomon reimplements them: `StrLen`, `MAlloc`, `Free`,
  `Print`, `MemCpy`, … keep their HolyC spellings.

---

## Language — solomon additions (not in HolyC)

- **Generics, monomorphized.** `class Vec<type T>`, parameter kinds `type` /
  `comparable T` / `int N`, generic functions and calls (`Sort<T>`, `Id<T>(x)`), the `:=`
  short-declaration (`n := expr`; `a, b := tuple`), **first-class tuples** (`(I64, F64)`
  for multi-return), and the compile-time `switch type(T) { case I64: … }`. HolyC has none
  of this — every container would be hand-rolled or macro-based.
- **`public` visibility + directory-scoped modules.** A top-level symbol is visible across
  its own directory; crossing a directory needs `public`. HolyC has no visibility or module
  system — everything is one global namespace.
- **Anonymous aggregate types** — an unnamed `class { … }` / `union { … }` may be used as
  a first-class type (variable, parameter, return, field). Aggregate typing is otherwise
  **nominal, like HolyC**: two same-shaped but differently-named types do *not* interchange
  — reinterpret with a pointer cast, a `union`, or `MemCpy`, exactly as in HolyC.
- **A real, strict front end**: lexer → preprocessor → parser → semantic analysis (name
  resolution + type inference) → layout, with diagnostics, run before any codegen.

---

## Language — missing from solomon (present in HolyC)

- **Inline assembly** (`asm { … }`) — the keyword is *reserved but unimplemented*; using
  it is an error rather than emitting machine code.
- **Register hints** `reg` / `noreg`, and **`lastclass`** — likewise reserved but
  unimplemented.
- **`I0`** (the zero-width signed type) — solomon has only `U0`.
- **Implicit/global symbol resolution.** In TempleOS every call resolves against the live
  global symbol table (the whole OS shares one incrementally-compiled address space).
  solomon requires every call to be known: **an unknown call is a compile error**, with no
  implicit-`extern` fallback. (`extern`/`import` are reserved but not wired up.)
- HolyC niceties solomon does not (currently) replicate, e.g. sub-integer member access
  (poking an int's bytes/words through union-style fields) and some DolDoc-aware string
  conveniences.

---

## Runtime & standard library — the big divide

In TempleOS the library is the kernel; in solomon it is a portable, C-shaped set of
modules (see [`vs-c-stdlib.md`](vs-c-stdlib.md)). Everything TempleOS-specific is absent —
not as a gap to fill, but because solomon targets ordinary OSes:

**Missing (the TempleOS OS API):**
- **DolDoc** — the hypertext/graphics document format that is TempleOS's terminal, files,
  and UI. `$$`-commands, colored/clickable text, embedded sprites: none of it.
- **2D graphics** — `GrPlot`/`GrLine`/`GrRect`/`Sprite`/`DCFill`/`GrPrint`, the framebuffer.
- **Sound** — `Snd`, `PlaySimple`, the music/note API.
- **The cooperative task model** — `Spawn`, the `Adam`/system tasks, task-local
  `Fs`/`Gs` segments, `Sleep` yielding to the scheduler, inter-task messaging.
- **The filesystem API** — `Cd`/`Dir`/`DirMk`/`FileFind`/`FileRead`/`FileWrite` over RedSea,
  `::/…` paths, the DolDoc-backed files.
- **Console/REPL/hardware** — `GetChar`/`ScanKey`, autocomplete, the JIT REPL, port
  `In`/`Out`, interrupts, the live `Compile`/patch-on-the-fly compiler API.

**Different (where solomon provides a portable stand-in):**
- **Concurrency**: real OS threads (`Thread`/`Join`, `Mutex`/`Cond`/`RwLock`, atomics) via
  pthreads/`clone(2)`, instead of TempleOS cooperative tasks. `Fs` still exists, but it is
  **per-OS-thread TLS** holding just the `CTask` exception state — not a full task object.
- **Allocation**: `MAlloc(size)` only; HolyC's `MAlloc(size, task)` task-owned-heap form
  has no analog (there are no task heaps).
- **I/O**: file-descriptor I/O (`Open`/`Read`/`Write`/`Close`) and BSD sockets, returning
  `-errno` as the value — versus TempleOS's DolDoc/RedSea calls.
- **Output target**: `Print` writes bytes to stdout (a normal stream), not into a DolDoc
  window; float/`%g` formatting is solomon's own correctly-rounded formatter, identical
  byte-for-byte on the interpreter and every backend.

**Added** (beyond what HolyC's library offered): the generic `Vec<T>`/`Hmap<K,V>`
containers, the math *special functions* (erf/gamma/Bessel) and a full `<math.h>`-grade
elementary set, a correctly-rounded `atof`/float formatter that needs no libc, and a
high-level TCP/`HttpGet` helper.

---

## Targets & toolchain

| | original HolyC | solomon |
|---|---|---|
| Host | TempleOS only, ring-0 | macOS, Linux, Windows; or freestanding |
| Backend | live JIT/AOT in-kernel, x86-64 | hand-rolled AArch64 + x86-64 codegen, no LLVM/IR; plus a tree-walking interpreter as the conformance oracle |
| Output | patched into the running OS | Mach-O (via `cc`), freestanding static ELF (raw syscalls, no libc/linker), self-contained Windows PE |
| Type checking | loose / permissive | strict semantic analysis before codegen |
| Symbol resolution | global table, incremental | whole-program; unknown calls error |

In short: solomon keeps HolyC's *feel* — PascalCase names, bare-string printing,
no-parens calls, `class`/`union`, sub-switches, range compares, exceptions, `#exe` — and
*adds* a modern type system with generics and modules, while *trading* the TempleOS
operating environment for portable, multi-target native binaries.
