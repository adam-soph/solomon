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
`#include "file"` keeps this property: the preprocessor maintains a *stack* of
`Lexer`s (one per open file) and pulls from the innermost, so included files
stream in without buffering. The `Lexer` owns its source bytes (so an included
file's lexer can outlive the `&str` it was built from). Paths resolve relative
to the including file's directory; `parser::parse(src)` defaults that to the CWD,
`parse_in_dir(src, dir)` sets it (the CLI passes the input file's parent).

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
  indexing, and comparison work. `MAlloc` of a scalar type and every `union`
  instance are instead **raw byte buffers** (`Region::Heap` / `Value::Union`)
  accessed through `Place::Bytes`, so type punning and overlapping union fields
  match the native byte layout. `run_to_string` is the safe "check then run"
  entry. **This interpreter is the conformance oracle for the arm64 backend** —
  when adding native-backend features, match its observable output.
- **`backend/arm64.rs`** — hand-emits AArch64 machine code (no LLVM/Cranelift/C),
  writes a Mach-O relocatable object by hand, and links with the system `cc`.
  Codegen is type-directed (uses `Expr::ty()` + the layout pass). It targets
  `aarch64-apple-darwin` specifically.

### builtins: one source of truth

`builtins.rs` is the single registry of built-in functions (`all()`,
`is_builtin()`, `libc_symbol()`). **sema seeds its signatures from here; the
interpreter dispatches behavior from here; the arm64 backend lowers them via
`libc_symbol()`.** Adding/altering a builtin means touching this module so the
backends stay in sync. Current set: `Print` (→ `printf`), `StrPrint` (→
`sprintf`, returns dst), `CatPrint` (sprintf-append, into `dst+strlen(dst)`) and
`MStrPrint` (asprintf-style: `snprintf` to measure, `malloc`, `sprintf`); number
conversion `I64ToStr`/`F64ToStr` (→ `sprintf` fixed format) and `StrToI64` (→
`atoll`) / `StrToF64` (→ `atof`); strings
`StrLen`/`StrCmp`/`StrNCmp`/`StrCpy`/`StrNCpy`/`StrCat`/`StrFind`/`StrChr`/`StrLastChr`/`StrSpn`/`StrCSpn`,
in-place `StrToUpper`/`StrToLower`/`StrRev` (inline loops); `Abs` (→
`llabs`); memory
`MemCpy`/`MemMove`/`MemSet`/`MemCmp`/`MemFind`/`MemSearch`/`MAlloc`/`Free`; char `ToUpper`/`ToLower`; and math
`Sqrt`/`Sin`/`Cos`/`Tan`/`ASin`/`ACos`/`ATan`/`ATan2`/`Pow`/`Exp`/`Ln`/`Log10`/
`Floor`/`Ceil`/`Round`/`Fabs`/`Sign`; plus `RandU64`. `NULL`/`TRUE`/`FALSE` are
const builtins seeded in each. `Print`/`StrPrint`/`CatPrint`/`MStrPrint` are
*not* in `libc_symbol`: all are special-cased in the arm64 backend
(`gen_print`/`gen_formatted_write`/`gen_mstrprint`) to translate the format
string (`translate_format`) and pass variadic args on the stack (Apple ABI); the
interpreter renders them with the shared `fmt` module. `I64ToStr`/`F64ToStr`
(`gen_tostr`) likewise lower to `sprintf` with a fixed format. `StrToUpper`/
`StrToLower` (`gen_str_case`) and `StrRev` (`gen_str_rev`) are also special —
inline loops, no `strupr`/`strrev` in libc.
Each *libc-backed* builtin keeps its HolyC
signature 1:1 with the libc one (e.g. `StrFind(haystack, needle)` ≙ `strstr`), so
it lowers through the generic path with no special-casing. Two *computed*
builtins have no libc equivalent and are emitted inline by the arm64 backend:
`Sign` (`(x>0)-(x<0)`), and `RandU64` — a deterministic splitmix64 (`builtins::
splitmix64`, fixed zero seed) so its sequence is identical in both backends; the
native backend keeps the state in a hidden `RNG_STATE_GLOBAL` common symbol the
interpreter mirrors with `Interpreter::rng_state`. The arm64 backend reaches libc
through a generic
external-symbol mechanism (`SymRef::Extern("_sym")` + `Asm::bl_extern`); the
Mach-O writer emits one undefined symbol per referenced libc function, so adding
a libc-backed builtin needs no new relocation plumbing. Two conformance notes:
`StrCmp` is normalized to a sign in `{-1,0,1}` in *both* backends (libc's
magnitude is unspecified — the native side reduces it with `sxtw`+`cset`); and
`MAlloc` in the interpreter is **byte-addressable** for integer/float element
types: it returns a `Region::Heap(Vec<u8>)` raw buffer, and a typed access
serializes `sizeof(T)` bytes (`Place::Bytes`), so aliasing the same buffer
through different scalar pointer types — **type punning** (`U8 *b = p` over an
`I64*`) — behaves like the native heap. Heap pointer arithmetic scales by the
element size (`ptr_step`/`heap_ptr_arith`). Class/pointer-element allocations
still use cell-backed `Region::Array` (no serialization needed), so heap linked
lists and arrays-of-classes work too. `try_typed_malloc` keys off `T *p =
MAlloc(...)` / `p = MAlloc(...)` to pick the representation from the target type.

`token.rs` similarly generates the `Keyword` enum + `from_str`/`as_str`/`is_type`
from one `keywords!` table to avoid drift.

## HolyC / implementation semantics worth knowing

- Default integer is `I64`; there is no `F32` (only `F64`). A bare string
  statement prints itself; `"fmt", a, b` is printf-style. A bare function name
  is a call (`Main;` runs `Main()`).
- **Calls must resolve** to a defined function or a registered builtin — an
  unknown call is a compile-time error (no implicit-extern fallback).
- **`switch`** takes `switch (x)` or the bracketed `switch [x]` (parsed
  identically). A body may carry `start:` / `end:` sub-labels (the `Start`/`End`
  keywords, `StmtKind::SwitchStart`/`SwitchEnd`): `start:` is a **prologue** run
  on entry before dispatch, `end:` an **epilogue** reached by fall-through that a
  `break` skips. Sema (`validate_switch_labels`) enforces ≤1 of each, `start:`
  before every case and `end:` after; both backends partition the body the same
  way (interp `exec_stmts_range`, arm64 `gen_switch`). Because they're keywords,
  `start`/`end` cannot be identifiers. The arm64 backend lowers a dense switch to
  an O(1) **jump table** (`try_gen_branch_table`) — when every case folds to a
  compile-time integer constant (`const_eval_i64` handles int/char literals,
  unary ops, and constant `+ - * / % & | ^ <<`) and the value span is small/
  dense, it emits a `span`-entry table of 32-bit offset words
  (`table[k] = label_k - table`, via the `Fixup::TableRel` data fixup — a
  section-internal distance needing no Mach-O relocation). Dispatch is
  `idx = v - min`, an unsigned bounds check (`COND_HI`), then
  `LDRSW off, [table, idx, lsl #2]; BR (table + off)`. Gaps/out-of-range and
  overlapping ranges resolve exactly as the compare-chain fallback does; sparse
  or non-constant switches keep the linear compare-chain.
- **Scalar stores coerce to the lvalue type** in the interpreter (`coerce_to` in
  `eval_init`/`eval_assign`): `I64 w = 3.14;` truncates to `3` and `F64 x = 5;`
  widens to `5.0`, matching the native backend (which truncates/widens in
  registers). Without this the interpreter kept the source `Value` and diverged
  on later arithmetic.
- **Narrow integers (`U8`/`I8`/`U16`/`I16`/`U32`/`I32`) follow C width rules** in
  both backends: arithmetic promotes to `I64` (no mid-expression wrap — `U8 a =
  200; a + 100` is `300`), then the value truncates to the declared width at each
  boundary: on store (interp `coerce_to`/`cast_value`; native store width or an
  explicit cast), on **argument passing** (interp coerces the arg to `p.ty` in
  `call`; native spills at the param width), and on **return** (interp coerces to
  `f.ret`; native `gen_cast(&cur_ret)` — `SBFM`/`UBFM`). So `U8 f(){return 300;}`
  yields `44`. The arg-truncation case was an interp-only gap before this.
- **`>>`, `/`, `%` are signedness-directed** in both backends (C semantics),
  keyed off the left operand's `ty()` (the lvalue's type for the `>>=`/`/=`/`%=`
  compound forms), threaded through `apply_binop` (interp) and `emit_int_binop`
  (native) as a `signed` flag (default signed — HolyC's default int is `I64`):
  `>>` is arithmetic for signed / logical for unsigned (`wrapping_shr` vs a `u64`
  shift; `ASRV` vs `LSRV`); `/` and `%` are signed/unsigned division
  (`wrapping_div`/`rem` vs `u64` ops; `SDIV` vs `UDIV`). So `-8 >> 1 == -4`,
  `0x8000…U64 >> 4` shifts in zeros, and `0x8000…U64 / 2 == 0x4000…`.
  `const_eval_i64` folds these the same way for the jump table. (`<<` is identical
  either way.)
- **Relational compares `< > <= >=` are signedness-directed**, but by the usual
  arithmetic conversions: **unsigned if either operand is unsigned** (vs the
  left-operand rule for shift/divide). The interpreter compares as `i64` or `u64`
  (`apply_binop`); the native backend picks signed (`LT/GT/LE/GE`) vs unsigned
  (`LO/HI/LS/HS`) condition codes (`gen_binary`). Both also compare integers at
  **full 64-bit width** — `==`/`!=` (`values_equal`) and the `<`-family no longer
  route through `f64`, which silently lost precision past 2^53 (e.g. `2^53+1 ==
  2^53` wrongly read true). F64 operands still compare as floats in both.
- **Float→integer conversion is signedness-directed**: an unsigned integer target
  uses `FCVTZU` / `f as u64` (saturating; negatives clamp to 0), a signed one
  `FCVTZS` / `f as i64` — they differ past `I64::MAX`, so `U64 u = 1e19` gives
  `10000000000000000000`, not the signed-saturated `I64::MAX`. The interpreter
  keys off the target type in `cast_value` (reached by every `coerce_to` site and
  by explicit casts); the native backend, where `gen_expr` has no target type,
  routes the int-store sites (var init, assignment, aggregate element, return,
  call argument) and explicit casts through `gen_int_expr(e, target)`.
- HolyC has no `struct` keyword: the aggregate is `class` (a plain `repr(C)` data
  type) plus `union`. The codebase follows this — the interpreter's aggregate
  value is `Value::Class` and helpers are named `class_*` (the Rust `struct`
  keyword still appears, but only for actual Rust structs, never a HolyC type).
  Classes/unions use `repr(C)` layout (natural alignment, declaration order, no
  reordering). Classes and array-by-reference behave by **C semantics**: a class
  is passed/assigned **by value** (deep copy), arrays **decay to pointers** (by
  reference).
- **Unions embed in classes.** A `union { … };` is anonymous — its members are
  *promoted* into the enclosing class (`obj.field`); a `union Name { … } m;` (or
  a `union Name m;` over a predefined union) is a named member (`obj.m.field`).
  The parser turns each embedded union into a synthetic top-level type (named or
  `$anonN`) injected via `Parser::pending_types`; the layout *promotes* an
  anonymous union's members into the parent at the union's offset
  (`is_anon_field`), so the native backend (offset-driven) and `sema::lookup_field`
  resolve them, and the interpreter's member access flattens through them. (Only
  unions embed this way — not anonymous classes.)

## The arm64 backend's self-consistent ABI

Because solomon functions only ever call each other (no class crosses the C
boundary; `_main` takes none), the native backend uses an internally consistent
ABI rather than full AAPCS64 register-packing:

- Integer/pointer args in `x0–x7`, **F64 args in `v0–v7`**, the two classes
  numbered independently. F64 values live in the FP register file (`v16`/`v17`
  as the expression evaluator's result/temp), spilled around calls via a GPR.
- **Classes by value**: an aggregate rvalue is represented by its *address*; a
  class arg is passed as a pointer in a GP register and the callee copies it
  into a local slot (`gen_memcpy`). Class **returns** use an sret pointer in
  `x8` (the callee writes the result through it). Whole-class assignment and
  copy-init are memcpies.
- **Array parameters** decay to pointers: the slot holds a pointer to the
  caller's data and the `VarLoc` is marked `indirect` (so `gen_addr_ident` loads
  the pointer); the declared array type is kept for index-stride math.
- Strings live inside `__text` and are addressed with self-resolved `ADR` (no
  relocation); globals are Mach-O **common symbols** (the linker allocates them).
  The relocations emitted are `bl`/`blr`-via-`bl` to libc externs (BRANCH26) and
  global `ADRP`/`ADD` (PAGE21/PAGEOFF12).
- **Function pointers**: `&Func` is a self-resolved `ADR` to the function's
  `__text` label (no relocation, like strings). An indirect call (`fp(args)`)
  spills the callee address, evaluates args through the normal ABI, then `BLR`s
  it; the callee's `Type::FuncPtr` signature drives arg classing and the return.

Print formatting is shared: `src/fmt.rs` parses each `%[flags][width][.prec]
[length]conv` spec once, and both backends consume it — the native side rebuilds
a libc format string (`to_c_format`, injecting the `ll` length so 64-bit args
print in full), the interpreter renders the value itself (`render_int`/
`render_str`) applying the same flags/width/precision. So `%-08.3x`, `%+5d`,
`%o`, `%*d`, full-width `%u`/`%x`, `%f` (six decimals by default, like libc), and
the scientific/general float forms `%e`/`%E`/`%g`/`%G` (`render_exp`/`render_g` —
Rust's `{:e}` does the correctly-rounded mantissa, then the exponent is restyled
to libc's `e±dd`) all agree. There is no longer any known formatting divergence
between the backends; all the `examples/*.hc` programs compile and run natively
with byte-identical output.

## Status note

The README's "Not yet implemented: code generation" line is stale — the arm64
native backend (`--build`) now exists and compiles the whole implemented HolyC
subset, including the `offset` keyword, brace aggregate initializers
(`I64 a[] = {1,2,3}`, `Pt p = {1,2}`, nested and partial), designated class
initializers (`Pt p = {.x = 1, .y = 2}`, out-of-order, partial, and nested), and
member access on a call result (`Mk().x`, including nested paths and F64 fields).
`#include "file"` is resolved (read + spliced, relative to the including file,
with cycle/depth guards), and a first slice of the core library exists as
builtins (`Abs`, `Sqrt`, `StrLen`, `StrCmp`, `StrCpy`, `MAlloc`, `Free`; see the
`builtins.rs` note above). Function pointers work end to end: the `ret (*name)
(types)` declarator (in var decls and as callback parameters), `&Func` to take a
function's address, and calls through a pointer (`fp(args)`) — the native backend
emits `ADR`+`BLR` for indirect calls (`Type::FuncPtr`). This extends to the
dispatch-table / vtable patterns: function-pointer **class fields**
(`s.method(args)`), **arrays** of function pointers (`I64 (*ops[])(...) = {&A,
&B}`, indexed and called), and brace-initialising them with `&Func`.
`typedef <type> <name>;` registers a type alias — including function-pointer
aliases (`typedef I64 (*BinOp)(I64,I64);`) which also make the "function
returning a function pointer" form usable (`BinOp Pick(){...}`). Aliases are
resolved at parse time (`Parser::type_aliases`, in `parse_base_type`), so they
never reach the AST as `Named` types and must be defined before use; they are
not hoisted (the C rule). The bracketed `switch [x]` form and `start:` / `end:`
sub-labels (prologue/epilogue) are lowered in both backends. Still genuinely
absent: most of the TempleOS core/standard library and DolDoc.

The worked HolyC programs live in `examples/*.hc` (top-level), listed once in
`tests/common/mod.rs` (`common::EXAMPLES`) and exercised by `tests/examples.rs`
(tokenize/parse/sema/run-clean over the whole list), `tests/programs.rs` (exact
interpreter output for the substantive ones), and `tests/arm64.rs`, whose
`native_matches_interp_for_every_example` compiles **every** example with the
native backend and asserts byte-for-byte equality with the interpreter — the
catch-all that keeps new examples and backend changes honest.
