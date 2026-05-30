# solomon

A reimplementation of **HolyC** — the C-like language created by Terry A. Davis
for [TempleOS](https://templeos.org) — written from scratch in Rust.

solomon takes HolyC source through a full compiler front end (lexer →
preprocessor → parser → semantic analysis → type layout) and runs it with a
tree-walking interpreter. The interpreter is structured as one *backend* behind
a small `Backend` trait, so other backends (a bytecode VM, native codegen, …)
can be added later without touching the front end.

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
$ solomon hello.hc
Hello, World!
x=42 y=255
```

## Status

Working today:

- **Lexer** — numbers (decimal/hex/binary, `F64` floats), strings with escapes,
  multi-char `'AB'` character constants, `//` and `/* */` comments, full
  operator set. Streams tokens on demand.
- **Preprocessor** — object-like and function-like `#define`, `#undef`, nested
  macro expansion (with a hide-set guard against runaway recursion), and
  `#ifdef` / `#ifndef` / `#else` / `#endif` conditionals.
- **Parser** — recursive descent with precedence-climbing expressions. A
  two-pass design hoists `class`/`union` names first, so a type can be used
  before it is defined.
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
  multidimensional and pass-by-reference), structs/unions, casts, and HolyC's
  implicit print.

Not yet implemented: code generation (interpreter only for now), the TempleOS
core/standard library, `#include` file resolution (the directive parses but
does not load files), and DolDoc.

## Building

Requires **Rust 1.85+** (the crate uses the 2024 edition). With Cargo:

```sh
cargo build --release     # binary at target/release/solomon
cargo test                # run the test suite
cargo run -- FILE.hc      # build and run a HolyC file
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

Building for an OS other than the host needs a cross linker/toolchain. The
simplest route is the [`cross`](https://github.com/cross-rs/cross) tool
(Docker-based):

```sh
cargo install cross
make all CARGO=cross
```

A native macOS host can build both Apple targets directly after `make targets`.
Override the target list with `make all TARGETS="x86_64-unknown-linux-gnu ..."`.

## Usage

```
solomon [--tokens | --ast | --check | --run] [FILE]
```

Reads from `FILE`, or from stdin if no file is given. Modes:

| Flag        | Does                                                            |
| ----------- | -------------------------------------------------------------- |
| `--run`     | type-check then execute (the default)                          |
| `--check`   | parse + semantic analysis; report errors, run nothing          |
| `--ast`     | parse and dump the AST                                         |
| `--tokens`  | run the lexer only and dump the token stream (no preprocessing)|

```sh
$ solomon --check broken.hc
semantic error at 2:3: call to undeclared function `DrawRect`
1 error(s)

$ echo 'I64 Sq(I64 x){ return x*x; } "%d\n", Sq(9);' | solomon
81
```

## Language notes

A few things specific to HolyC (and to this implementation):

- The default integer type is `I64`; there is no `F32` (only `F64`).
- A bare string statement prints itself, and `"fmt", a, b` is printf-style
  (`%d %u %x %X %c %s %f %p %%`). A bare function name is a call: `Main;` runs
  `Main()`.
- `'A'` is `0x41`; multi-character constants pack little-endian (`'AB'` is
  `0x4241`).
- Structs use `repr(C)` layout (natural alignment, declaration order).
- Calls must resolve to a defined function or a built-in (`Print`); there is no
  implicit-extern fallback yet.

See `tests/data/*.hc` for worked examples, from `hello.hc` up to a linked list,
a stack-machine interpreter, shape-area dispatch via inheritance, a
preprocessor-heavy math library, and 3×3 matrix multiplication.

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
  builtins.rs   built-in function registry (shared by sema + interpreter)
  backend.rs    the Backend trait + error type
  backend/
    interp.rs   tree-walking interpreter
  main.rs       CLI
tests/          lexer, parser, sema, preproc, layout, interpreter, and
                whole-program (sample file) tests
```
