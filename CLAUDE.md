# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

solomon is a from-scratch reimplementation of **HolyC** (Terry Davis's TempleOS
language) in Rust. It is a real compiler front end plus two backends: a
tree-walking interpreter and a hand-rolled AArch64 native-code generator.

## Commands

```sh
cargo build                 # debug build (binary: target/debug/solomon)
cargo build --release       # release build
cargo test                  # whole suite
cargo test --test arm64     # one integration-test file (each tests/<name>.rs is a crate)
cargo test compiles_floats  # a single test by name (substring match)
cargo fmt                   # format (CI-relevant: keep the tree fmt-clean)
cargo clippy --all-targets  # lint — see note below
```

Run a HolyC program through each mode (default is `--run`):

```sh
cargo run -- --run FILE.hc            # type-check then interpret
cargo run -- --check FILE.hc          # parse + sema only, report errors
cargo run -- --ast FILE.hc            # dump the parsed AST
cargo run -- --tokens FILE.hc         # raw lexer output (no preprocessing)
cargo run -- --build -o out FILE.hc   # compile to a native binary via the arm64 backend
echo 'I64 Sq(I64 x){return x*x;} "%d\n", Sq(9);' | cargo run --   # reads stdin if no FILE
```

`make` wraps cargo for cross-compilation (`make`, `make all`, `make <triple>`,
`make dist`, `make macos-universal`); use `CARGO=cross` for non-host OSes. See
the Makefile / README for the target list.

### Important testing notes

- **The arm64 backend tests (`tests/arm64.rs`) only run on an `aarch64-apple-darwin`
  host with `cc` available** — they shell out to compile + link + execute real
  Mach-O binaries (~14s). On any other platform they self-skip (print "skipping"
  and pass), so a green `cargo test` elsewhere does **not** mean the native
  backend was exercised. Verify arm64 changes on an Apple-silicon Mac.
- Clippy reports pre-existing stylistic lints across the codebase that are *not*
  enforced; `cargo build` is warning-free and that is the bar. Don't "fix" the
  whole tree — just keep your own additions from adding new warnings.

## Architecture

### The pipeline

Source flows through one direction, each stage a separate module:

```
lexer → preprocessor → parser → sema → layout → backend
```

`parser::parse(src)` is the front-end entry point and is **two-pass**: it first
`hoist_type_names` (streams the preprocessed tokens just to collect `class`/`union`
names) so a type can be used before it is defined, then does the real parse.
`sema::check_program(&Program) -> Vec<SemaError>` runs name resolution + type
inference. `layout::compute(&Program)` computes `repr(C)` sizes/offsets. The CLI
(`main.rs`) wires these together per mode.

### Streaming tokens (a load-bearing design constraint)

Tokens are **never materialized into a list**. `lexer.rs` defines a
`TokenStream` trait (`next_token()`), implemented by both `Lexer` and
`Preprocessor<S>`. `Parser<S: TokenStream>` is generic over the stream and pulls
tokens lazily through a small look-ahead buffer. The preprocessor is itself a
`TokenStream` that wraps the lexer, so `Parser` sees a `Preprocessor<Lexer>`.
Preserve this: do not add code that collects the full token stream up front.

### Typed AST

`ast.rs` nodes are `Expr { kind, span, ty }` / `Stmt { kind, span }`. Two things
to know:

- `Expr.ty` is a `RefCell<Option<Type>>`. Sema annotates **every** expression
  with its inferred type via interior mutability (accessors `ty()`/`set_ty()`),
  so all later passes keep immutable `&Program` APIs. Backends read `e.ty()` to
  drive type-directed codegen and `sizeof(expr)`.
- `PartialEq` on AST nodes is hand-written to **ignore spans** (and `ty`), so
  tests can build expected trees with `Span::dummy()`.

`sema`'s `check_ident` returns a variable's **true, undecayed** type; array→pointer
decay is applied at use sites (binary/index/deref). Backends must account for
this (e.g. an array-typed `Ident` means "address of the array data").

### Backends

`backend.rs` defines `trait Backend { name(); run(&Program) }`. Construction is
backend-specific (the interpreter takes an output sink; the arm64 backend takes
an output path), but both run uniformly. `Backend::run` assumes an
already-checked program.

- **`backend/interp.rs`** — tree-walking `Interpreter<W: Write>`. Values are
  `Rc<RefCell<Value>>` cells; pointers use a region+offset model so arithmetic,
  indexing, and comparison work. `run_to_string` is the safe "check then run"
  entry. **This interpreter is the conformance oracle for the arm64 backend** —
  when adding native-backend features, match its observable output.
- **`backend/arm64.rs`** — hand-emits AArch64 machine code (no LLVM/Cranelift/C),
  writes a Mach-O relocatable object by hand, and links with the system `cc`.
  Codegen is type-directed (uses `Expr::ty()` + the layout pass). It targets
  `aarch64-apple-darwin` specifically.

### builtins: one source of truth

`builtins.rs` is the single registry of built-in functions (`all()`,
`is_builtin()`). **sema seeds its signatures from here; the interpreter
dispatches behavior from here.** Adding/altering a builtin means touching this
module so the two stay in sync (currently just `Print`; `NULL`/`TRUE`/`FALSE`
are const builtins seeded in each).

`token.rs` similarly generates the `Keyword` enum + `from_str`/`as_str`/`is_type`
from one `keywords!` table to avoid drift.

## HolyC / implementation semantics worth knowing

- Default integer is `I64`; there is no `F32` (only `F64`). A bare string
  statement prints itself; `"fmt", a, b` is printf-style. A bare function name
  is a call (`Main;` runs `Main()`).
- **Calls must resolve** to a defined function or a registered builtin — an
  unknown call is a compile-time error (no implicit-extern fallback).
- Structs/unions use `repr(C)` layout (natural alignment, declaration order, no
  reordering). Structs and array-by-reference behave by **C semantics**: structs
  are passed/assigned **by value** (deep copy), arrays **decay to pointers**
  (by reference).

## The arm64 backend's self-consistent ABI

Because solomon functions only ever call each other (no struct crosses the C
boundary; `_main` takes none), the native backend uses an internally consistent
ABI rather than full AAPCS64 register-packing:

- Integer/pointer args in `x0–x7`, **F64 args in `v0–v7`**, the two classes
  numbered independently. F64 values live in the FP register file (`v16`/`v17`
  as the expression evaluator's result/temp), spilled around calls via a GPR.
- **Structs by value**: an aggregate rvalue is represented by its *address*; a
  struct arg is passed as a pointer in a GP register and the callee copies it
  into a local slot (`gen_memcpy`). Struct **returns** use an sret pointer in
  `x8` (the callee writes the result through it). Whole-struct assignment and
  copy-init are memcpies.
- **Array parameters** decay to pointers: the slot holds a pointer to the
  caller's data and the `VarLoc` is marked `indirect` (so `gen_addr_ident` loads
  the pointer); the declared array type is kept for index-stride math.
- Strings live inside `__text` and are addressed with self-resolved `ADR` (no
  relocation); globals are Mach-O **common symbols** (the linker allocates them).
  The only relocations emitted are `bl _printf` (BRANCH26) and global
  `ADRP`/`ADD` (PAGE21/PAGEOFF12).

Known output difference vs the interpreter: C's `printf("%f")` always prints six
decimals (`3.140000`), while the interpreter trims (`3.14`). This is formatting
only; the numeric values agree. All ten `tests/data/*.hc` samples compile and
run natively.

## Status note

The README's "Not yet implemented: code generation" line is stale — the arm64
native backend (`--build`) now exists and compiles the whole implemented HolyC
subset. Still genuinely absent: the TempleOS core/standard library, `#include`
file resolution (parses but doesn't load), DolDoc, and array/aggregate
*initializers* (`I64 a[] = {...}` is currently a parse error).
