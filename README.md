<div align="center">

# Solomon

**A from-scratch reimplementation of [HolyC](https://templeos.org) — Terry A. Davis's TempleOS language — in Rust.**

A full compiler front end, a tree-walking interpreter, and three hand-rolled native code generators. No LLVM, no Cranelift, no assembler.

[![Test](https://github.com/adam-soph/solomon/actions/workflows/test.yml/badge.svg)](https://github.com/adam-soph/solomon/actions/workflows/test.yml)
![Rust 2024](https://img.shields.io/badge/Rust-2024_edition-CE412B?logo=rust&logoColor=white)
![Codegen](https://img.shields.io/badge/codegen-hand--rolled-success)
![Native targets](https://img.shields.io/badge/native_targets-3-blue)
![Linux](https://img.shields.io/badge/Linux-freestanding_static_ELF-orange)
![Conformance](https://img.shields.io/badge/examples-18%2F18_byte--identical-brightgreen)

</div>

---

solomon takes HolyC source through a full compiler front end (lexer →
preprocessor → parser → semantic analysis → type layout) and either interprets it
with a tree-walking **interpreter** or compiles it with one of several hand-rolled
native code generators (behind a small `Codegen` trait) named for their target —
**`aarch64-apple-darwin`** (emits a Mach-O object, no LLVM/Cranelift, links with
`cc`) and the **`x86_64-unknown-linux`** / **`aarch64-unknown-linux`** Linux targets
(each writes a **freestanding static ELF** with raw syscalls — no linker, no libc).
A codegen backend is an (architecture, OS) pair, since the object format, syscalls,
and ABI depend on the OS, not just the CPU. The interpreter is the conformance
oracle the native backends match byte-for-byte (on all 18 example programs).

```holyc
U0 Main()
{
  "Hello, World!\n";
  I64 x = 42, y = 0xFF;
  "x=%d y=%d\n", x, y;
}
Main;
```

```console
$ hci hello.hc
Hello, World!
x=42 y=255
```

## Contents

- [The pipeline](#the-pipeline)
- [Status](#status)
- [Building](#building)
- [Usage](#usage)
- [Language notes](#language-notes)
- [Examples](#examples)
- [Project layout](#project-layout)

## Status

### Front end

- **Lexer** — numbers (decimal/hex/binary, `F64` floats), strings with escapes,
  multi-char `'AB'` character constants, `//` and `/* */` comments, full
  operator set. Streams tokens on demand.
- **Preprocessor** — object-like and function-like `#define`, `#undef`, nested
  macro expansion (with a hide-set guard against runaway recursion),
  `#ifdef` / `#ifndef` / `#else` / `#endif` conditionals, `#include "file"`
  (relative to the including file) and `#include <name>` (resolved against the
  standard-library search path), both with cycle/depth guards.
- **Parser** — recursive descent with precedence-climbing expressions. A
  two-pass design hoists `class`/`union` names first, so a type can be used
  before it is defined. Handles function-pointer declarators
  (`I64 (*fp)(I64)`), `typedef` aliases, and brace/designated initializers.
- **Semantic analysis** — name resolution and scoping, type inference (it
  produces a *typed* AST), member/field checking with single inheritance, call
  arity (including default and variadic arguments), lvalue and control-flow-context
  checks, `goto`/label scoping, and `return` checking. Calls to unknown
  functions are compile-time errors.
- **Type layout** — `repr(C)`-style sizes, alignment, and field offsets, used by
  `sizeof` and available to backends.

### Backends

| Target | Output | Linker / libc | Default on |
| ------ | ------ | ------------- | ---------- |
| `aarch64-apple-darwin` | Mach-O relocatable object | links with `cc` | Apple silicon |
| `x86_64-unknown-linux` | freestanding static ELF | none — raw syscalls | Linux x86-64 |
| `aarch64-unknown-linux` | freestanding static ELF | none — raw syscalls | Linux ARM64 |

Both Linux targets are **freestanding** — no libc, no linker, raw syscalls. (Darwin is the one hosted target: macOS has no stable syscall ABI, so it links libSystem via `cc`.)

- **Interpreter** — executes the program, including recursion, all loop
  forms, `switch` with `case lo ... hi:` ranges, `goto`, real pointer semantics
  (arithmetic, indexing, comparison, `&`/`*`/`->`), arrays (including
  multidimensional and pass-by-reference), classes/unions, casts, function
  pointers, a byte-addressable `MAlloc` heap, and HolyC's implicit print.
- **`aarch64-apple-darwin`** (the default build on Apple silicon, or
  `--target aarch64-apple-darwin`) — hand-emits machine code and a Mach-O
  relocatable object, then links with `cc`. Type-directed codegen covering the
  whole implemented subset: control flow (dense `switch`es lower to an O(1)
  jump table), functions (recursion, default and variadic args), classes by
  value (sret returns), F64, function pointers (`ADR`+`BLR` for indirect calls),
  brace/designated initializers, and calls into libc for the built-in library.
- **`x86_64-unknown-linux`** (`--target x86_64-unknown-linux`) —
  hand-writes a **freestanding static ELF** with raw Linux syscalls: no linker,
  no libc, no relocations. Its own `_start` runs the program and `exit`s. Covers
  the same subset (integers with C signedness, pointers/arrays, classes/unions
  incl. sret, globals, F64, `switch`/`goto`, and printf with correctly-rounded
  `%f`/`%e`/`%g`), with the core-library built-ins re-implemented from scratch
  (an `mmap` bump allocator, inline string/memory loops, splitmix64).
- **`aarch64-unknown-linux`** (`--target aarch64-unknown-linux`) —
  also a **freestanding static ELF**, sharing the entire AArch64 emitter with the
  Darwin backend but emitting its own `_start`, raw syscalls, and runtime (no libc,
  no linker). Globals self-address to a fixed BSS slot; the runtime re-implements the printf
  engine including the **correctly-rounded bignum `%f`/`%e`/`%g`**, the `mmap` bump
  allocator, and the string/memory built-ins. All 18 examples run byte-for-byte
  identical to the interpreter under `docker --platform linux/arm64`.

### Core library

A slice of the **core library** exists as built-ins, shared by all backends:
`Print`, formatted-string builders `StrPrint`/`CatPrint`/`MStrPrint`, string ops
(`StrLen`, `StrCmp`/`StrNCmp`, `StrCpy`/`StrNCpy`, `StrCat`, `StrFind`,
`StrChr`/`StrLastChr`, `StrSpn`/`StrCSpn`, `StrToUpper`/`StrToLower`, `StrRev`),
number conversion (`StrToI64`/`StrToF64`, `I64ToStr`/`F64ToStr`), memory (`MemCpy`,
`MemMove`, `MemSet`, `MemCmp`, `MemFind`, `MemSearch`, `MAlloc`, `Free`), char
(`ToUpper`, `ToLower`), the exactly-reproducible float ops (`Abs`/`Sign`/`Fabs`,
`Sqrt`, `Floor`/`Ceil`/`Round`), and a deterministic PRNG (`RandU64`).

> **Why no `Sin`/`Cos`/`Pow`/`Exp`/`Ln`?** The transcendental math functions are
> deliberately **not** built-ins: every built-in has a portable, solomon-defined
> meaning, whereas a transcendental's value would be only "whatever the host libm
> computes" (not reproducible across platforms, and impossible in a freestanding
> target). They belong in a future HolyC standard library with a defined algorithm.

**Not yet implemented:** most of the TempleOS core/standard library and DolDoc.

## Building

Requires **Rust 1.85+** (the crate uses the 2024 edition). With Cargo:

```sh
cargo build --release             # binaries at target/release/{hcc,hci}
cargo test                        # run the test suite
cargo run --bin hci -- FILE.hc    # interpret a HolyC file
cargo run --bin hcc -- FILE.hc -o app   # compile it to a native binary
```

### Cross-compiling

A `Makefile` wraps Cargo to build release binaries for several OS/architecture
targets:

```sh
make                       # build for the host machine
make targets               # rustup target add every supported triple (run once)
make all                   # build every target
make aarch64-apple-darwin  # build a single target
make dist                  # build all + collect binaries into dist/
make macos-universal       # arm64 + x86_64 fat binary (macOS host)
make help                  # list everything
```

**Releases** are published by the `Release` GitHub Actions workflow
(`.github/workflows/release.yml`): push a version tag (`git tag v0.1.0 && git push
--tags`) or run it from the Actions tab, and it builds every target on a matching
native runner and uploads the binaries to the GitHub release — no local Docker.

Default targets:

| Platform              | Triple                        |
| --------------------- | ----------------------------- |
| macOS (Apple silicon) | `aarch64-apple-darwin`        |
| macOS (Intel)         | `x86_64-apple-darwin`         |
| Linux x86-64          | `x86_64-unknown-linux-gnu`    |
| Linux ARM64           | `aarch64-unknown-linux-gnu`   |
| Linux x86-64 (static) | `x86_64-unknown-linux-musl`   |
| Windows x86-64        | `x86_64-pc-windows-gnu`       |
| Windows x86           | `i686-pc-windows-gnu`         |

These triples are what the **`hcc`/`hci` binaries themselves** are compiled for —
every one can interpret HolyC (`hci`). Native code *generation* (`hcc`) is a
separate axis, and only four targets have a backend: `aarch64-apple-darwin`,
`x86_64-unknown-linux`, `aarch64-unknown-linux` (both freestanding static ELFs),
and `x86_64-pc-windows`. The Linux targets link no libc, so there are no `-gnu`/
`-musl` codegen variants — those suffixes are only meaningful as the *Rust* build
triples above (how `hcc` itself is compiled). On any other platform `hci`
interprets HolyC but `hcc` cannot emit a native executable yet.

Building for an OS other than the host needs a cross linker/toolchain. The
Makefile uses the [`cross`](https://github.com/cross-rs/cross) tool (Docker-based)
for foreign-OS targets automatically, and plain `cargo` for host-OS targets, so a
single `make all` builds every triple:

```sh
# Install cross from git — the 0.2.5 release predates rustup 1.28 and has no
# Apple-silicon images, so it fails on a modern macOS host.
cargo install cross --git https://github.com/cross-rs/cross
make all
```

On a macOS host that means both Apple targets build with cargo (after
`make targets`) and the Linux/Windows targets build with `cross` (which needs
Docker running). Override the tools with `CARGO=...` / `CROSS=...`, or the
target list with `make all TARGETS="x86_64-unknown-linux-gnu ..."`.

## Usage

There are two binaries: **`hci`** runs a HolyC program, **`hcc`** compiles one.
Both read from `FILE`, or from stdin if no file is given.

```text
hci [FILE] [ARGS...]                     run with the tree-walking interpreter
hcc [--target TRIPLE] [-o OUT] [FILE]    compile a native binary (the default)
hcc <subcommand> [FILE]
```

`hci` runs the program; arguments after `FILE` become its `argv` (read via
`ArgC`/`ArgV`). `hcc` with no subcommand compiles a native binary for the host
(`-o OUT`, default `a.out`); `--target TRIPLE` cross-compiles instead. Its
subcommands are front-end tools:

| Command      | Does                                                       |
| ------------ | ---------------------------------------------------------- |
| `hci FILE`   | type-check then execute with the tree-walking interpreter  |
| `hcc FILE`   | compile a native binary for the host target (`-o OUT`)     |
| `hcc check`  | parse + semantic analysis; report errors, run nothing      |
| `hcc ast`    | parse and dump the AST                                      |
| `hcc tokens` | run the lexer only and dump the token stream               |

`--target` accepts `aarch64-apple-darwin`, `x86_64-unknown-linux`,
`aarch64-unknown-linux`, and `x86_64-pc-windows`. The Linux targets are
freestanding (no libc), so the `-gnu`/`-musl` libc suffixes are not accepted —
use the bare triple.

```console
$ hcc check broken.hc
semantic error at 2:3: call to undeclared function `DrawRect`
1 error(s)

$ echo 'I64 Sq(I64 x){ return x*x; } "%d\n", Sq(9);' | hci
81
```

## Language notes

A few things specific to HolyC (and to this implementation):

- The default integer type is `I64`; there is no `F32` (only `F64`).
- A bare string statement prints itself, and `"fmt", a, b` is printf-style:
  conversions `%d %i %u %x %X %o %c %s %f %e %E %g %G %p %%` with the usual flags,
  width, and precision (`%-08.3d`, `%+5x`, `%.2f`, `%.3e`, `%*d`). Values are
  64-bit. A bare function name is a call: `Main;` runs `Main()`.
- `'A'` is `0x41`; multi-character constants pack little-endian (`'AB'` is
  `0x4241`).
- HolyC has no `struct` keyword: aggregates are `class` (the struct-equivalent —
  plain `repr(C)` data, natural alignment, declaration order) and `union`. A
  `class`/`union` passes and assigns by value, while arrays decay to pointers.
  Unions can be embedded in a class — anonymously (`union {...};`, members
  promoted to the parent) or as a named member (`union Bits {...} b;`).
- Aggregates can be brace-initialized positionally (`I64 a[] = {1,2,3}`,
  `Pt p = {1,2}`, nested/partial) or with designators (`Pt p = {.x=1, .y=2}`).
- Function pointers (`I64 (*fp)(I64,I64) = &Add; fp(3,4)`) work as variables,
  callback parameters, class fields, and dispatch-table arrays. `typedef`
  registers type aliases, including readable function-pointer ones.
- Calls must resolve to a defined function or a built-in; there is no
  implicit-extern fallback. The native backend lowers built-ins to libc.
- Relational operators **chain** as range comparisons: `5 < i < 10` means
  `5 < i && i < 10`, and `0 <= a <= b < n` reads naturally. Parentheses opt out
  (`(a < b) < c`); `==`/`!=` keep their plain C meaning. (Interior operands are
  duplicated, so keep them side-effect-free.)
- `switch` accepts both `switch (x)` and the bracketed `switch [x]`, and a body
  may carry `start:` / `end:` sub-labels: `start:` is a prologue run on entry
  before dispatch, `end:` an epilogue reached by fall-through (a `break` skips
  it). Because of this, `start` and `end` are **reserved words** — they cannot be
  used as identifiers (`I64 start;` is a parse error).

## Examples

The `examples/` directory has 18 worked HolyC programs, from `hello.hc` up to a
recursive-descent JSON parser. Every one runs under the interpreter and compiles
on each native backend with byte-identical output.

| File | What it shows |
| ---- | ------------- |
| `hello.hc` | the basics — prints, variable declarations, top-level statements |
| `fib.hc` | functions, recursion, and loops |
| `control.hc` | `switch`/`case` ranges, `do`-`while`, ternary, bit ops, casts, `sizeof` |
| `classes.hc` | `class` and `union` definitions, pointers, member access |
| `shapes.hc` | class inheritance, upcasting to a base pointer, tagged dispatch |
| `linklist.hc` | an array-backed singly linked list with sorted insertion |
| `vm.hc` | a tiny stack machine — bytecode `(opcode, argument)` interpreter |
| `matrix.hc` | fixed-size 3×3 matrix math with nested arrays and loops |
| `preproc.hc` | macros, conditionals, and a forward type reference (hoisting) |
| `mathlib.hc` | a preprocessor-heavy macro math library |
| `builtin.hc` | a tour of the core-library built-ins (string/memory/math) |
| `text.hc` | text processing — word counting and `StrFind` search |
| `args.hc` | the command line via the ambient `ArgC`/`ArgV` globals |
| `varargs.hc` | variadic functions via the ambient `VargC`/`VargV` locals |
| `wordcount.hc` | a word-frequency analyzer over the generic `Vec<T>`/`Hmap<K,V>` stdlib (inferred type args) |
| `hashmap.hc` | a string→`I64` hash map with separate chaining |
| `shuffle.hc` | a Fisher–Yates shuffle of `0..N-1` driven by `RandU64` |
| `json.hc` | recursive-descent JSON parser → heap tree → re-serialize with `switch [tag]` |
| `report.hc` | a formatted sales report with aligned columns (`StrPrint`/`CatPrint`) |
| `gallery.hc` | numbers rendered in every conversion — decimal/hex/octal/fixed/`%e`/`%g` |

## Standard library

The standard library is written **in HolyC** and lives in `lib/`. Because it's
ordinary HolyC built on the deterministic `F64` ops and the algebraic builtins, it
computes identically on the interpreter and every native backend — the same
byte-for-byte reproducibility the transcendentals were kept out of the builtin set
to guarantee (a builtin needs a portable, solomon-defined value; a host libm's
`Sin` doesn't have one, but a HolyC `Sin` with a *defined algorithm* does).

Pull a module in with an **angle include**:

```holyc
#include <math.hc>
U0 Main() { "%.6f\n", Exp(1.0); }   // 2.718282
Main;
```

The library is **embedded into the compiler at build time** (each `lib/*.hc` is
`include_str!`'d into the binary), so `hcc`/`hci` are **self-contained** — no `lib/`
directory is needed at runtime, and a single copied binary just works. `#include
<name>` resolves first against an optional override search path — the
`SOLOMON_STDLIB` environment variable (`:`-separated) or `hcc -I DIR` — and then the
embedded copy, so you can point at a working tree's `lib/` for development without
rebuilding the compiler. (`#include "file"` is unchanged: relative to the including
file.)

The modules: `lib/cstr.hc` (C-style `U8 *` string ops — `StrLen`/`StrCmp`/`StrCpy`/
`StrFind`/…, number conversion incl. `F64ToStr`), `lib/mem.hc` (the
heap intrinsics `MAlloc`/`Free`/`HeapExtend`/`MSize`, the `Mem*` family, `ReAlloc`),
`lib/fmt.hc` (the printf family `Print`/`StrPrint`/`CatPrint`/`MStrPrint`),
`lib/time.hc` (the clock `UnixNS`/`NanoNS`/`Sleep` + calendar math), `lib/ctype.hc` (`ToUpper`/`ToLower` and
the `Is*` predicates), `lib/vec.hc` (`Vec`, a generic growable array),
`lib/strconv.hc` (`StrToF64`, a correctly-rounded bignum `atof`) over `lib/bignum.hc`
(a minimal arbitrary-precision integer), and `lib/rand.hc` (the deterministic
splitmix64 `RandU64` + `SeedRand`). The printf/heap/clock modules are **intrinsics** —
declared in lib, but the compiler emits their lowering (the builtin registry is now
just `ArgC`/`ArgV`/`VarArg*`). The math library is layered as `lib/bits.hc` (the IEEE
bit/classification ops — `Float64bits`/`Float64frombits`/`IsNaN`/`IsInf`/`Signbit`/
`NaN`/`Inf`/`Copysign`) → `lib/math.hc` (elementary) → `lib/special.hc` (Erf/Gamma/
Bessel). `lib/math.hc` provides the rounding ops
`Floor`/`Ceil`/`Round`/`RoundToEven`/`Trunc`/`Mod`(`Fmod`), `Exp`/`Log`(`Ln`)/`Log2`/
`Log10`/`Exp2`/`Expm1`/`Log1p`/`Pow`/`Pow10`/`Cbrt`, the trig/inverse-trig
`Sin`/`Cos`/`Tan`/`Sincos`/`Atan`/`Asin`/`Acos`/`Atan2`, the hyperbolics + inverses
`Sinh`/`Cosh`/`Tanh`/`Asinh`/`Acosh`/`Atanh`, `Hypot`, exact `PowI`,
`Gcd`/`Factorial`/`Min`/`Max`/`Clamp`, `FMA`, a **correctly-rounded `Sqrt`** (Newton +
a Dekker exact-residual correction, bit-identical to the hardware `fsqrt`), and a
Go-`math`-style surface — IEEE bit ops (`Float64bits`/`Float64frombits`),
classification (`IsNaN`/`IsInf`/`Signbit`/`NaN`/`Inf`/`Copysign`), exponent
(`Frexp`/`Ldexp`/`Logb`/`Ilogb`), `Modf`/`Dim`/`Remainder`/`Nextafter`, and the
error-function / gamma family (`Erf`/`Erfc`/`Erfinv`/`Erfcinv`/`Gamma`/`Lgamma` —
Taylor + continued-fraction erf, Winitzki+Newton inverses, Lanczos g=7 gamma), and
the Bessel functions (`J0`/`J1`/`Jn`/`Y0`/`Y1`/`Yn` — power/log series for small x,
asymptotic amplitude/phase beyond, Miller downward recurrence for `Jn`) — all
matching libm to ~10 decimals. All reproducible by construction, so *no* float op is
a builtin (the rounding/abs/sqrt ones are **optimization intrinsics** — a backend
emits the FP instruction in place, falling back to the HolyC body). `lib/time.hc` adds calendar math
on the `UnixNS` clock: `FromUnix`/`ToUnix` (Hinnant's civil↔days, exact for any
date), `FmtISO` (`YYYY-MM-DD HH:MM:SS`), `IsLeap`, and an impure `Now`.

## Project layout

```text
lib/            the HolyC standard library (angle-included: `#include <math.hc>`)


```text
src/
  token.rs      token + keyword definitions
  lexer.rs      lexer (streaming) + TokenStream trait
  preproc.rs    preprocessor (macros + conditionals), a TokenStream
  ast.rs        the (typed) AST
  parser.rs     recursive-descent parser, generic over a TokenStream
  sema.rs       semantic analysis / type checking
  layout.rs     type size/alignment/offset pass
  builtins.rs   built-in function registry (shared by all backends)
  interp.rs     tree-walking interpreter (the conformance oracle)
  codegen.rs    the Codegen trait + CodegenError (the shared backend interface)
  arm64/        AArch64 backend — asm.rs (encoder), mod.rs (codegen), darwin.rs
                (Mach-O + cc), linux.rs (freestanding static ELF)
  x86_64/       x86-64 backend — asm.rs (encoder), mod.rs (codegen + OsTarget),
                linux.rs (static ELF), windows.rs (self-contained PE)
  bin/          the executables (auto-discovered by Cargo):
    hcc.rs      the `hcc` compiler CLI
    hci.rs      the `hci` interpreter CLI
tests/          lexer, parser, sema, preproc, layout, interpreter, the native
                backends, target registration, and whole-program tests
```
