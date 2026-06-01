# Solomon

A reimplementation of **HolyC** — the C-like language created by Terry A. Davis
for [TempleOS](https://templeos.org) — written from scratch in Rust.

solomon takes HolyC source through a full compiler front end (lexer →
preprocessor → parser → semantic analysis → type layout) and either interprets it
with a tree-walking **interpreter** or compiles it with one of two hand-rolled
native code generators (behind a small `Codegen` trait) named for their target —
**`aarch64-apple-darwin`** (emits a Mach-O object, no LLVM/Cranelift, links with
`cc`) and **`x86_64-unknown-linux`** (writes a freestanding static ELF with raw
syscalls — no linker, no libc). A codegen backend is an (architecture, OS) pair,
since the object format, syscalls, and ABI depend on the OS, not just the CPU.
The interpreter is the conformance oracle the native backends match byte-for-byte.

```holyc
U0 Main()
{
  "Hello, World!\n";
  I64 x = 42, y = 0xFF;
  "x=%d y=%d\n", x, y;
}
Main;
```

```
$ holyc run hello.hc
Hello, World!
x=42 y=255
```

## Status

Working today:

- **Lexer** — numbers (decimal/hex/binary, `F64` floats), strings with escapes,
  multi-char `'AB'` character constants, `//` and `/* */` comments, full
  operator set. Streams tokens on demand.
- **Preprocessor** — object-like and function-like `#define`, `#undef`, nested
  macro expansion (with a hide-set guard against runaway recursion),
  `#ifdef` / `#ifndef` / `#else` / `#endif` conditionals, and `#include "file"`
  (resolved relative to the including file, with cycle/depth guards).
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
- **Interpreter backend** — executes the program, including recursion, all loop
  forms, `switch` with `case lo ... hi:` ranges, `goto`, real pointer semantics
  (arithmetic, indexing, comparison, `&`/`*`/`->`), arrays (including
  multidimensional and pass-by-reference), classes/unions, casts, function
  pointers, a byte-addressable `MAlloc` heap, and HolyC's implicit print.
- **`aarch64-apple-darwin` native backend** (the default build on Apple silicon,
  or `--target aarch64-apple-darwin`) — hand-emits machine code and a Mach-O
  relocatable object, then links with `cc`. Type-directed codegen covering the
  whole implemented subset: control flow (dense `switch`es lower to an O(1)
  jump table), functions (recursion, default and variadic args), classes by
  value (sret returns), F64, function pointers (`ADR`+`BLR` for indirect calls),
  brace/designated initializers, and calls into libc for the built-in library.
- **`x86_64-unknown-linux` native backend** (`--target x86_64-unknown-linux`) —
  hand-writes a **freestanding static ELF** with raw Linux syscalls: no linker,
  no libc, no relocations. Its own `_start` runs the program and `exit`s. Covers
  the same subset (integers with C signedness, pointers/arrays, classes/unions
  incl. sret, globals, F64, `switch`/`goto`, and printf with correctly-rounded
  `%f`/`%e`/`%g`), with the core-library built-ins re-implemented from scratch
  (an `mmap` bump allocator, inline string/memory loops, splitmix64).

A slice of the **core library** exists as built-ins, shared by all backends:
`Print`, formatted-string builders `StrPrint`/`CatPrint`/`MStrPrint`, string ops
(`StrLen`, `StrCmp`/`StrNCmp`, `StrCpy`/`StrNCpy`, `StrCat`, `StrFind`,
`StrChr`/`StrLastChr`, `StrSpn`/`StrCSpn`, `StrToUpper`/`StrToLower`, `StrRev`),
number conversion (`StrToI64`/`StrToF64`, `I64ToStr`/`F64ToStr`), memory (`MemCpy`,
`MemMove`, `MemSet`, `MemCmp`, `MemFind`, `MemSearch`, `MAlloc`, `Free`), char
(`ToUpper`, `ToLower`), the exactly-reproducible float ops (`Abs`/`Sign`/`Fabs`,
`Sqrt`, `Floor`/`Ceil`/`Round`), and a deterministic PRNG (`RandU64`).

The transcendental math functions (`Sin`/`Cos`/`Pow`/`Exp`/`Ln`/…) are
deliberately **not** built-ins: every built-in has a portable, solomon-defined
meaning, whereas a transcendental's value would be only "whatever the host libm
computes" (not reproducible across platforms, and impossible in a freestanding
target). They belong in a future HolyC standard library with a defined algorithm.

Not yet implemented: most of the TempleOS core/standard library and DolDoc.

## Building

Requires **Rust 1.85+** (the crate uses the 2024 edition). With Cargo:

```sh
cargo build --release         # binary at target/release/holyc
cargo test                    # run the test suite
cargo run -- run FILE.hc      # interpret a HolyC file
cargo run -- FILE.hc -o app   # compile it to a native binary for the host
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

Default targets:

| Platform            | Triple                        |
| ------------------- | ----------------------------- |
| macOS (Apple silicon) | `aarch64-apple-darwin`      |
| macOS (Intel)       | `x86_64-apple-darwin`         |
| Linux x86-64        | `x86_64-unknown-linux-gnu`    |
| Linux ARM64         | `aarch64-unknown-linux-gnu`   |
| Linux x86-64 (static) | `x86_64-unknown-linux-musl` |
| Windows x86-64      | `x86_64-pc-windows-gnu`       |
| Windows x86         | `i686-pc-windows-gnu`         |

These triples are what the **`holyc` binary itself** is compiled for — every one
runs the front end and the interpreter (`holyc run`). Native code *generation*
(the default build, or `--target`) is a separate axis: `aarch64-apple-darwin`,
`x86_64-unknown-linux` (its `-gnu` and `-musl` triples are the same freestanding
static ELF), `x86_64-pc-windows`, and `aarch64-unknown-linux-gnu`/`-musl` (an ELF
linked with an aarch64 gcc — dynamically against glibc, or statically against
musl) have a backend. On any other platform `holyc` interprets HolyC but cannot
emit a native executable yet.

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

```
holyc [--target TRIPLE] [-o OUT] [FILE]   compile a native binary (the default)
holyc <subcommand> [FILE]
```

Reads from `FILE`, or from stdin if no file is given. With **no subcommand**,
`holyc` compiles a native binary for the host's architecture and OS (`-o OUT`,
default `a.out`); `--target TRIPLE` cross-compiles instead. The subcommands
select other behavior:

| Command         | Does                                                       |
| --------------- | ---------------------------------------------------------- |
| *(none)*        | compile a native binary for the host target (`-o OUT`)     |
| `run`           | type-check then execute with the tree-walking interpreter  |
| `check`         | parse + semantic analysis; report errors, run nothing      |
| `ast`           | parse and dump the AST                                      |
| `tokens`        | run the lexer only and dump the token stream               |

`--target` accepts `aarch64-apple-darwin`, `x86_64-unknown-linux`,
`x86_64-pc-windows`, and `aarch64-unknown-linux-gnu` — each with `-gnu`/`-musl`
forms where applicable.

```sh
$ holyc check broken.hc
semantic error at 2:3: call to undeclared function `DrawRect`
1 error(s)

$ echo 'I64 Sq(I64 x){ return x*x; } "%d\n", Sq(9);' | holyc run
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

See `examples/*.hc` for worked examples, from `hello.hc` up to a linked list,
a stack-machine interpreter, shape-area dispatch via inheritance, a
preprocessor-heavy math library, 3×3 matrix multiplication, a core-library tour
(`builtin.hc`) exercising the string/memory/math built-ins, a heap-growing dynamic
array (`vector.hc`), text processing with `StrFind` (`text.hc`), a string-keyed
hash map with chaining (`hashmap.hc`), a `RandU64`-driven shuffle
(`shuffle.hc`), and a recursive-descent JSON parser that builds a heap tree of
tagged nodes — objects, arrays, strings (with escapes), F64 reals, integers,
and `true`/`false`/`null` — then re-serializes it back to JSON with a `switch
[tag]` pretty-printer (`json.hc`). Two `StrPrint`/`CatPrint` showcases round it
out: a formatted sales report with aligned columns (`report.hc`) and a gallery
rendering numbers in every conversion — decimal/hex/octal/fixed/`%e`/`%g`
(`gallery.hc`).

## Project layout

```
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
  codegen.rs    the Codegen trait + CodegenError
  codegen/
    arm64_darwin.rs   aarch64-apple-darwin code generator + Mach-O writer
    x86_64_linux.rs   x86_64-unknown-linux code generator + static-ELF writer
  main.rs       CLI
tests/          lexer, parser, sema, preproc, layout, interpreter, and the two
                native backends (arm64_darwin, x86_64_linux) + whole-program tests
```
