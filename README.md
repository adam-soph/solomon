<div align="center">

# Solomon

**A from-scratch reimplementation of [HolyC](https://templeos.org) — Terry A. Davis's TempleOS language — in Rust.**

A full compiler front end, a tree-walking interpreter, and four hand-rolled native code generators. No LLVM, no Cranelift, no assembler.

[![Test](https://github.com/adam-soph/solomon/actions/workflows/test.yml/badge.svg)](https://github.com/adam-soph/solomon/actions/workflows/test.yml)
![Rust 2024](https://img.shields.io/badge/Rust-2024_edition-CE412B?logo=rust&logoColor=white)
![Codegen](https://img.shields.io/badge/codegen-hand--rolled-success)
![Native targets](https://img.shields.io/badge/native_targets-4-blue)
![Linux](https://img.shields.io/badge/Linux-freestanding_static_ELF-orange)
![Conformance](https://img.shields.io/badge/examples-18%2F18_byte--identical-brightgreen)

</div>

---

solomon takes HolyC source through a full compiler front end — lexer, preprocessor,
parser, semantic analysis, and type layout. It then either **interprets** the program
with a tree-walking interpreter or **compiles** it with one of several hand-rolled
native code generators behind a small `Codegen` trait, each named for its target:

| Target | Output | Linker / libc |
| ------ | ------ | ------------- |
| `aarch64-apple-darwin` | Mach-O object | links with `cc` |
| `x86_64-unknown-linux` | freestanding static ELF | none — raw syscalls |
| `aarch64-unknown-linux` | freestanding static ELF | none — raw syscalls |
| `x86_64-pc-windows` | self-contained PE | none — `kernel32` imports |

A backend is an (architecture, OS) pair, since the object format, syscalls, and
ABI depend on the OS, not just the CPU. Both Linux targets are **freestanding** —
no libc, no linker, raw syscalls — with their own `_start`; the Windows target
likewise emits a self-contained PE with hand-built `kernel32` imports and no
linker. (Darwin is the one hosted target: macOS has no stable syscall ABI, so it
links libSystem via `cc`.)
The interpreter is the conformance oracle, and the three executable backends match
it byte-for-byte on all 18 example programs. The Windows PE can't run on the test
host, so it is verified by byte-scanning its emitted code instead.

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
$ hcc -i hello.hc
Hello, World!
x=42 y=255
```

## Status

### Front end

- **Lexer** — numbers (decimal/hex/binary, `F64` floats), strings with escapes,
  multi-char `'AB'` character constants, `//` and `/* */` comments, the full
  operator set. Streams tokens on demand.
- **Preprocessor** — object- and function-like `#define`, `#undef`, nested macro
  expansion, `#ifdef`/`#ifndef`/`#else`/`#endif`, and `#include` (both `"file"`
  and stdlib `<name>` forms), with cycle/depth guards.
- **Parser** — recursive descent with precedence-climbing expressions. Two-pass,
  so a type can be used before it is defined. Handles function-pointer
  declarators, `typedef` aliases, brace/designated initializers, and generics.
- **Semantic analysis** — name resolution, type inference (producing a *typed*
  AST), member/field checking with single inheritance, call arity, lvalue and
  control-flow checks, `goto`/label scoping. Unknown calls are compile errors.
- **Type layout** — `repr(C)`-style sizes, alignment, and field offsets.
- **Generics** — `class Vec<T>` / `Hmap<K,V>` and generic functions are
  monomorphized in a dedicated pass between parse and sema, with type-directed
  inference so call sites usually need no explicit type args.

### Backends

- **Interpreter** — executes the program, including recursion, all loop forms,
  `switch` with `case lo ... hi:` ranges, `goto`, real pointer semantics, arrays
  (multidimensional and pass-by-reference), classes/unions, casts, function
  pointers, a byte-addressable `MAlloc` heap, and HolyC's implicit print.
- **`aarch64-apple-darwin`** (default on Apple silicon) — hand-emits machine code
  and a Mach-O object, then links with `cc`. Type-directed codegen over the whole
  implemented subset: control flow (dense `switch`es lower to an O(1) jump table),
  functions, classes by value (sret returns), F64, and function pointers.
- **`x86_64-unknown-linux`** / **`aarch64-unknown-linux`** — each hand-writes a
  **freestanding static ELF** (own `_start`, raw syscalls, no linker, no libc, no
  relocations) covering the same subset, including printf with correctly-rounded
  bignum `%f`/`%e`/`%g`, an `mmap` bump allocator, and the core-library built-ins.
  The AArch64 one shares its entire emitter with the Darwin backend.
- **`x86_64-pc-windows`** — hand-writes a **self-contained PE** with `kernel32`
  imports (`WriteFile`/`GetStdHandle`/`VirtualAlloc`/…) resolved through a
  hand-built import address table, no linker. Shares the x86-64 emitter with the
  Linux backend behind an `OsTarget` seam that swaps syscalls for the OS calls.

**Not yet implemented:** most of the TempleOS core/standard library and DolDoc.

## Installation

The fastest way to get `hcc` is the install script, which downloads the prebuilt
binary for your platform from the latest GitHub release and puts it on your PATH.
The standard library is embedded in the binary, so that single file is the whole
install.

**Linux / macOS** (and Windows under Git Bash / MSYS2 / WSL):

```sh
curl -fsSL https://raw.githubusercontent.com/adam-soph/solomon/main/install.sh | sh
```

**Windows** (native PowerShell):

```powershell
irm https://raw.githubusercontent.com/adam-soph/solomon/main/install.ps1 | iex
```

Each script detects your OS and architecture and picks the matching release asset
— the universal binary on macOS (Apple silicon + Intel), a static `musl`/`gnu` ELF
on Linux (x86_64 / aarch64), or the `.exe` on Windows (x86_64 / i686; ARM64 uses
the x86_64 build under emulation).

Both accept the same options — a release tag and an install directory — as flags or
via the `HCC_VERSION` / `HCC_INSTALL_DIR` environment variables:

```sh
./install.sh --version v0.1.0 --dir ~/.local/bin       # POSIX
```
```powershell
.\install.ps1 -Version v0.1.0 -Dir C:\tools\bin        # PowerShell
```

`install.sh` installs to `/usr/local/bin` when that's writable without `sudo`,
otherwise `~/.local/bin`, and prints the `export` line if the directory isn't on
your `PATH`. `install.ps1` installs to `%LOCALAPPDATA%\hcc\bin` and adds it to your
user `PATH` automatically. To build from source instead, see below.

## Building

Requires **Rust 1.85+** (2024 edition).

```sh
cargo build --release             # binary at target/release/hcc
cargo test                        # run the test suite
cargo run -- -i FILE.hc           # interpret a HolyC file
cargo run -- FILE.hc -o app       # compile it to a native binary
```

### Cross-compiling

A `Makefile` wraps Cargo to build release binaries across OS/architecture targets:

```sh
make                       # build for the host machine
make targets               # rustup target add every supported triple (run once)
make all                   # build every target
make aarch64-apple-darwin  # build a single target
make dist                  # build all + collect binaries into dist/
make macos-universal       # arm64 + x86_64 fat binary (macOS host)
make help                  # list everything
```

The Makefile uses [`cross`](https://github.com/cross-rs/cross) (Docker-based) for
foreign-OS targets and plain `cargo` for host-OS targets, so one `make all` builds
every triple. Install `cross` from git (the 0.2.5 release has no Apple-silicon
images):

```sh
cargo install cross --git https://github.com/cross-rs/cross
make all
```

The `hcc` binary is built for the host platform, and **every** build can *interpret*
HolyC (`hcc -i`). The interpreter emulates its POSIX-flavoured fds, files, and
process IDs cross-platform, so the tool runs on Windows too. Native code
*generation* is a separate axis; the targets with a backend are listed in the table
above. **Releases** are published by the `Release` GitHub Actions workflow: push a
version tag (`git tag v0.1.0 && git push --tags`) and it builds every target on a
matching native runner.

## Usage

There is one binary, **`hcc`**: with no subcommand it compiles a HolyC program, and
`hcc -i` interprets one. It reads from `FILE`, or from stdin if no file is given.

```text
hcc [--target TRIPLE] [-o OUT] [FILE]    compile a native binary (the default)
hcc -i [FILE] [ARGS...]                  run with the tree-walking interpreter
hcc <subcommand> [FILE]
```

`hcc` with no subcommand compiles a native binary for the host (`-o OUT`, default
`a.out`); `--target TRIPLE` cross-compiles. `hcc -i` executes the program with the
interpreter; arguments after `FILE` become its `argv` (read via `ArgC`/`ArgV`). The
front-end subcommands are:

| Command      | Does                                                       |
| ------------ | ---------------------------------------------------------- |
| `hcc FILE`   | compile a native binary for the host target (`-o OUT`)     |
| `hcc -i`     | type-check then execute with the tree-walking interpreter  |
| `hcc check`  | parse + semantic analysis; report errors, run nothing      |
| `hcc ast`    | parse and dump the AST                                      |
| `hcc tokens` | run the lexer only and dump the token stream               |

`--target` accepts `aarch64-apple-darwin`, `x86_64-unknown-linux`,
`aarch64-unknown-linux`, and `x86_64-pc-windows`. The Linux targets are
freestanding (no libc), so the `-gnu`/`-musl` libc suffixes are not accepted — use
the bare triple.

```console
$ hcc check broken.hc
semantic error at 2:3: call to undeclared function `DrawRect`
1 error(s)

$ echo 'I64 Sq(I64 x){ return x*x; } "%d\n", Sq(9);' | hcc -i
81
```

## Language notes

A few things specific to HolyC (and to this implementation):

- The default integer type is `I64`; there is no `F32` (only `F64`).
- A bare string statement prints itself, and `"fmt", a, b` is printf-style:
  conversions `%d %i %u %x %X %o %c %s %f %e %E %g %G %p %%` with the usual flags,
  width, and precision (`%-08.3d`, `%.2f`, `%*d`). A bare function name is a call:
  `Main;` runs `Main()`.
- `'A'` is `0x41`; multi-character constants pack little-endian (`'AB'` is
  `0x4241`).
- HolyC has no `struct` keyword: aggregates are `class` (plain `repr(C)` data,
  natural alignment, declaration order) and `union`. A `class`/`union` passes and
  assigns by value, while arrays decay to pointers. Unions can be embedded in a
  class anonymously (members promoted) or as a named member.
- Aggregates can be brace-initialized positionally (`I64 a[] = {1,2,3}`,
  `Pt p = {1,2}`) or with designators (`Pt p = {.x=1, .y=2}`).
- Function pointers (`I64 (*fp)(I64,I64) = &Add; fp(3,4)`) work as variables,
  callback parameters, class fields, and dispatch-table arrays. `typedef`
  registers type aliases, including function-pointer ones.
- Relational operators **chain** as range comparisons: `5 < i < 10` means
  `5 < i && i < 10`. Parentheses opt out; `==`/`!=` keep plain C meaning.
  (Interior operands are duplicated, so keep them side-effect-free.)
- `switch` accepts both `switch (x)` and `switch [x]`, and a body may carry
  `start:` / `end:` sub-labels (a prologue run on entry, an epilogue reached by
  fall-through that `break` skips). So `start`/`end` are reserved words.

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
| `wordcount.hc` | a word-frequency analyzer over the generic `Vec<T>`/`Hmap<K,V>` stdlib |
| `hashmap.hc` | a string→`I64` hash map with separate chaining |
| `shuffle.hc` | a Fisher–Yates shuffle of `0..N-1` driven by `RandU64` |
| `json.hc` | recursive-descent JSON parser → heap tree → re-serialize |
| `report.hc` | a formatted sales report with aligned columns (`StrPrint`/`CatPrint`) |
| `gallery.hc` | numbers in every conversion — decimal/hex/octal/fixed/`%e`/`%g` |

## Standard library

The standard library is written **in HolyC** and lives in `lib/`. Because it's
ordinary HolyC built on the deterministic `F64`/integer ops, it computes
identically on the interpreter and every native backend. Pull a module in with an
**angle include**:

```holyc
#include <math.hc>
U0 Main() { "%.6f\n", Exp(1.0); }   // 2.718282
Main;
```

The library is **embedded into the compiler at build time** (each `lib/*.hc` is
`include_str!`'d into the binary), so `hcc` is self-contained — no `lib/`
directory is needed at runtime. `#include <name>` resolves first against an
optional override path (`SOLOMON_STDLIB` env var or `hcc -I DIR`), then the
embedded copy.

A few primitives are **intrinsics** — declared in lib, but the compiler emits
their lowering because they bundle syscalls or the format machinery: the printf
family (`Print`/`StrPrint`/…, `lib/fmt.hc`), the heap (`MAlloc`/`Free`, the
prelude `lib/builtin.hc`), the clock (`lib/time.hc`), and I/O syscalls. The
rounding/abs/sqrt float ops are *optimization* intrinsics — a backend emits the FP
instruction where it can, else runs the HolyC body. Everything else is plain
HolyC. The modules:

| Module | Contents |
| ------ | -------- |
| `cstr.hc` | C-style `U8 *` string ops + number conversion |
| `mem.hc` | the `Mem*` family, `ReAlloc`, the heap intrinsics |
| `ctype.hc` | `ToUpper`/`ToLower` and the `Is*` predicates |
| `vec.hc` / `hmap.hc` | generic `Vec<T>` / `Hmap<K,V>` containers |
| `sort.hc` | generic quicksort + binary search |
| `bignum.hc` / `strconv.hc` | a minimal big integer + correctly-rounded `StrToF64` |
| `rand.hc` | the deterministic splitmix64 `RandU64` + `SeedRand` |
| `bits.hc` → `math.hc` → `special.hc` | IEEE bit ops → elementary functions (incl. a correctly-rounded `Sqrt`) → Erf/Gamma/Bessel |
| `time.hc` | the clock intrinsics + calendar math |
| `io.hc` / `net.hc` / `os.hc` | file, socket, and process/OS helpers over syscalls |
| `thread.hc` / `sync.hc` | threads, atomics, and a futex-based mutex/cond/rwlock |

> **Why no built-in `Sin`/`Cos`/`Pow`?** Every built-in needs a portable,
> solomon-defined value. A transcendental's would be only "whatever the host libm
> computes" — not reproducible across platforms, and impossible in a freestanding
> target. So they live in the library with a *defined* algorithm instead.

## Project layout

```text
lib/            the HolyC standard library (angle-included: `#include <math.hc>`)
src/
  token.rs      token + keyword definitions          (shared front-end output)
  ast.rs        the (typed) AST                       (shared front-end output)
  frontend/     the front-end passes: source → fully-concrete, type-checked AST + layouts
    lexer.rs    lexer (streaming) + TokenStream trait
    preproc.rs  preprocessor (macros + conditionals), a TokenStream
    parser.rs   recursive-descent parser, generic over a TokenStream
    mono.rs     monomorphization pass (generics)
    sema.rs     semantic analysis / type checking
    layout.rs   type size/alignment/offset pass
  intrinsics.rs lib-declared functions the backends lower specially
  ir.rs         the SSA IR (typed vregs, basic blocks, phi)
  lower/        AST → SSA IR lowering (on-the-fly SSA construction)
  irinterp.rs   the SSA IR interpreter (the conformance oracle) + run_to_* entry points
  backend.rs    the native-codegen layer: the Codegen trait + CodegenError, the
                IR-level shared driver (pure-IR analyses + the emit_blocks loop), and
                the out-of-SSA + linear-scan register-promotion pass
  arm64/        AArch64 backend — asm.rs (encoder), isel.rs (IR → machine code) +
                isel/{prims,heap,exc}.rs (OS prims / bump heap / try-throw unwinder),
                darwin.rs (Mach-O + cc), linux.rs (freestanding static ELF)
  x86_64/       x86-64 backend — asm.rs (encoder), isel.rs (IR → machine code) +
                isel/{prims,heap,exc}.rs (OS prims + Win32 calls / bump heap / unwinder),
                mod.rs (OsTarget seam), linux.rs (static ELF), windows.rs (PE)
  main.rs       the hcc CLI: compile (default) + the `-i` interpreter mode
tests/          lexer, parser, sema, preproc, layout, interpreter, the native
                backends, and whole-program tests
```
