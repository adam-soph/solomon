# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

solomon is a from-scratch reimplementation of **HolyC** (Terry Davis's TempleOS
language) in Rust: a real compiler front end, a tree-walking **interpreter**
(`src/interp.rs`, the **conformance oracle**), and hand-rolled native code generators
behind the **`Codegen`** trait (`src/codegen.rs`). A backend is an (arch, OS) pair, not
just a CPU: `aarch64-apple-darwin` (Mach-O via `cc`), `aarch64-unknown-linux` and
`x86_64-unknown-linux` (**freestanding** static ELFs — no libc, no linker, raw
syscalls), and `x86_64-pc-windows` (a self-contained PE with hand-built `kernel32`
imports). Darwin is the one hosted target. Every backend matches the interpreter
byte-for-byte on all examples.

## Commands

```sh
cargo build                 # debug (CLI: target/debug/hcc; the lib crate is `solomon`)
cargo build --release
cargo test                  # whole suite
cargo test --test arm64_darwin   # one integration-test crate (each tests/<name>.rs is a crate)
cargo test compiles_floats  # a single test by name substring
cargo fmt                   # keep the tree fmt-clean (a CI bar)
cargo clippy --all-targets  # lint (see note below)
```

One binary, **`hcc`** (`src/main.rs`, named via a `[[bin]]` in `Cargo.toml`): with no
subcommand it compiles a host-native binary (`-o OUT`, default `a.out`); `--target
TRIPLE` cross-compiles; **`-i FILE [args]`** interprets (the oracle — trailing args
become the program's `argv`, reads stdin if no FILE); `check`/`ast`/`tokens` are
front-end-only. Run via `cargo run -- …`. `make` wraps cargo for cross-compilation
(`make`/`make all`/`make <triple>`/`make dist`/`make macos-universal`; `CARGO=cross`
for non-host OSes). Release binaries are published by the `Release` GitHub Actions
workflow on a `v*` tag, not by `make`.

### Testing notes
- **Don't run docker tests locally.** The freestanding crates (`x86_64_linux`,
  `arm64_linux`) and the impure-feature crates (`io`/`net`/`thread`/`sync`) execute
  under docker on CI and self-skip locally. Run the non-docker crates + `arm64_darwin`.
- **`tests/arm64_darwin.rs` (and `tests/x86_64_linux.rs`) have two layers.**
  Structural checks byte-inspect the emitted object on **any** host (no toolchain);
  end-to-end checks shell out to `cc`/docker and execute, so they self-skip off the
  matching host. A green `cargo test` on an Apple-silicon Mac exercises the AArch64
  *emitter* but not Linux/x86 *execution* — verify those on the matching host / CI.
- **The bar is warning-free `cargo build`** plus fmt-clean. Clippy reports pre-existing
  unenforced lints; don't fix the tree, just don't add new warnings.

## Architecture

### Pipeline
`lexer → preprocessor → parser → mono → sema → layout → backend`, each a module.
`parser::parse(src)` is the entry point and is **two-pass**: `hoist_type_names` streams
the tokens to collect `class`/`union` names (so a type can be used before it's defined),
then the real parse, then the **`mono`** pass (`src/mono.rs`) which monomorphizes every
deferred generic into concrete AST — so everything downstream sees an ordinary,
**fully-concrete** `Program`. `sema::check_program` does name resolution + type
inference; `layout::compute` does `repr(C)` sizes/offsets; `src/main.rs` wires it per
mode. Entry points: `parse(src)` (CWD base, embedded stdlib only), `parse_in_dir`,
`parse_with(src, dir, search)`; the CLI passes the input file's parent +
`solomon::stdlib_dirs()` (the `SOLOMON_STDLIB` overrides; `-I DIR` prepends more).

**Float-formatter auto-include.** Float printing is a pure-HolyC function
(`lib/fltfmt.hc`'s `FmtFloat`) the backends *call*, not emit; with no dead-code
elimination it can't be linked unconditionally, so `hoist_type_names` also returns
`uses_float` (set when a format string carries `%f`/`%e`/`%E`/`%g`/`%G`, via the shared
`crate::fmt::parse`) and the prelude then prepends `#include <fltfmt.hc>`
(guard-deduped). Interp ignores it (renders via `fmt.rs`), Darwin compiles-but-uses-libc,
the freestanding/x86/Windows backends call it.

### Streaming tokens (a load-bearing constraint)
Tokens are **never materialized into a list.** `lexer.rs`'s `TokenStream` trait
(`next_token()`) is implemented by both `Lexer` and `Preprocessor<S>`; `Parser<S:
TokenStream>` pulls lazily through a small look-ahead buffer and sees a
`Preprocessor<Lexer>`. **Do not add code that buffers the full stream.** `#include` keeps
this: the preprocessor stacks `Lexer`s (one per open file) and pulls from the innermost.
Quoted `#include "file"` resolves relative to the including file; angle `#include <name>`
resolves against the search path, then the **stdlib embedded at build time**
(`EMBEDDED_STDLIB` in `lib.rs` — each `lib/*.hc` via `include_str!`, so the compiler is
self-contained; editing a `lib/*.hc` recompiles).

### Typed AST (`ast.rs`)
- `Expr { kind, span, ty }` / `Stmt { kind, span }`. `Expr.ty` is a
  `RefCell<Option<Type>>` — sema annotates **every** expression via interior mutability
  (`ty()`/`set_ty()`), so later passes keep immutable `&Program`. Backends read `e.ty()`
  for type-directed codegen + `sizeof(expr)`.
- `PartialEq` ignores spans (and `ty`), so tests build expected trees with `Span::dummy()`.
- `sema::check_ident` returns a variable's **undecayed** type; array→pointer decay is
  applied at use sites — an array-typed `Ident` means "address of the array data".

### No IR; the dedup seams (`src/backend.rs`)
There is **no IR** (no SSA/LLVM/Cranelift): each backend walks the typed AST and emits
machine code directly. Shared logic lives in `src/backend.rs`:
- **`trait Emitter`** — one TCC-style emitter vtable (assoc types `Place`/`Slot`),
  implemented per backend by its inner `Cg` worker. Backend-independent driver functions
  (`gen_init_into`, `gen_switch`/`gen_if`/`gen_while`/`gen_do_while`/`gen_for`,
  `gen_call`) call back into it for the leaf emits, so the backends can't drift on
  init/control-flow/call *shape*. Also holds the shared printf flag-bit ABI
  (`spec_flags`/`int_conv`/`F_*`) and `classify_args`.
- **`OsTarget`** (x86) / **`ArmTarget`** (arm64) seams — the per-OS deltas (exit, page
  alloc, std write, file ops, clock; freestanding vs hosted vs Windows). One `Cg` per
  arch serves both that arch's OS targets through the seam.

### Interpreter & backends
- **`interp.rs`** — tree-walking `Interpreter<W: Write>`, the **conformance oracle**;
  `run_to_string` is the safe check-then-run entry. **Match its observable output when
  adding backend features.** Values are `Rc<RefCell<Value>>` cells; pointers are
  region+offset. The exceptions are **raw byte buffers** (`Region::Heap`/`Value::Union`,
  via `Place::Bytes`): `MAlloc` of a scalar type, every `union`, and a generic container
  whose element is a class — so type punning and union aliasing match the native byte
  layout. A pointer stored into a byte buffer is serialised as an 8-byte handle into a
  `PtrTable` (a `PtrVal` is region+offset, not an address); a whole class value is
  (de)serialised field-by-field by layout (`store_bytes_value`/`load_bytes_value`, gated
  by `ty_is_aggregate`) — so `VecPush(&v, pt)` / `Pt p = VecAt(&v, i)` round-trip.
- **`arm64/mod.rs`** (+ `darwin.rs`/`linux.rs`) — hand-emits AArch64 + the Mach-O object,
  links with `cc` (Darwin); or a freestanding static ELF (`aarch64-unknown-linux`, via
  the `ArmTarget` seam: own `_start`, raw syscalls, `mmap` bump allocator). Type-directed.
  Emission-time optimizations (no separate pass, which would shift label/fixup offsets):
  constant folding, immediate-form arithmetic + power-of-two strength reduction
  (`try_imm_binop`), simple-operand lhs-in-register. Plus a real post-emission
  **peephole** (`Asm::peephole` — dead-`mov` removal/fusion over scratch x9/x10) and a
  per-function **register promotion** pass (`plan_registers` — linear scan over
  loop-depth-weighted live intervals; hot scalar locals → callee-saved x19–x28 / d8–d15).
  All behavior-preserving (the interpreter + all-examples native conformance test are the
  oracle).
- **`x86_64/mod.rs`** (+ `linux.rs`/`windows.rs`) — hand-emits x86-64 + a freestanding
  static ELF (`x86_64-unknown-linux`) or, via the `OsTarget` seam, a self-contained PE
  with hand-built kernel32 imports (`x86_64-pc-windows`). Stack machine in `rax`; System V
  arg regs; SSE2 F64. Same implemented subset as arm64 (control flow incl. `start:`/`end:`
  switch sub-labels; classes by value + sret; globals in BSS; function pointers; printf).
  Uses a compare-chain for `switch` where arm64 has an O(1) jump table.

Both backends cover the whole implemented subset; only the deliberately-excluded
transcendentals are absent (they're lib functions, below).

### intrinsics (the one compiler-provided-behaviour seam)
`intrinsics.rs` is the single seam: a **standard-library function declared in `lib/*.hc`**
(resolved like any call, *with* an `#include`) that the backends lower specially. (The old
`builtins.rs` registry is **deleted**.) Two flavours (`IntrinsicKind`):
- **Optimization** — has a real HolyC body a backend may replace with an instruction where
  supported, else calls the body (`Sqrt` → `fsqrt`/`sqrtsd`, else the lib Newton; both
  correctly rounded, so they agree). The interpreter always runs the body.
- **Primitive** — a body-less lib *prototype* the backend *must* lower (it bundles OS
  syscalls or the format machinery): the printf family, the heap
  (`MAlloc`/`Free`/`HeapExtend`/`MSize`), the clock, fd I/O + sockets, fs mutation +
  process control, threads, atomics.

Dispatch in both backends + interp gates on `intrinsics::is_primitive(name)`. A **compiled
user function shadows a like-named primitive** (a program's own `Read`/`Join`) — a body in
`funcs` means "call the body." Bare strings and the `"fmt", args` comma form lower inline
(not `Print` calls), so they need no include.

The only compiler-provided names with **no `lib/*.hc` declaration** are the implicit
command line `ArgC`/`ArgV`, environment `EnvP`, and a `...` function's `VargC`/`VargV` —
sema-injected globals/locals (doc-commented in `lib/builtin.hc`), captured at entry.
(Darwin's heap lowering keeps a tiny private `libc_symbol` map `MAlloc`→`_malloc`/
`Free`→`_free` in `arm64/mod.rs`.) Everything reducible is pure HolyC in `lib/*.hc`, so
each function computes identically on every target; each lib file has an `#ifndef _NAME_HC`
guard. The **impure groups** (clock, fd I/O, sockets, fs mutation, process ids, threads)
are conformance-tested by *property* (e.g. monotonic clock, write→read round-trip), never
interp-vs-native value.

### Standard library (`lib/*.hc`)
Each module is includable on its own; the human-readable header in each file is the API
reference. Map:
- `builtin.hc` — implicit prelude (no `#include`): `NULL`/`TRUE`/`FALSE`, `MAlloc`/`Free`
  prototypes, doc for the sema-injected `ArgC`/`ArgV`/`EnvP`/`VargC`/`VargV`.
- `cstr.hc` — C `U8 *` string ops (`<string.h>` family) + the number<->string pairs
  `StrToI64`/`I64ToStr` and `StrToF64`/`F64ToStr` + `CmpStr`. `StrToF64` is the
  correctly-rounded `atof` (over `Bn`); `F64ToStr` is its round-trip inverse (shortest
  `%g` that `StrToF64` parses back exactly, via `FmtFloat`). So `cstr.hc` now includes
  `<bignum.hc>` and `<fltfmt.hc>` (neither depends back on it — no cycle; the old `%g`
  `F64ToStr` lived in `<fmt.hc>` precisely to avoid the printf-core cycle).
  `mem.hc` — `mem*` family, `HeapExtend`/`MSize` (heap intrinsics), `CAlloc`,
  `ReAlloc`. `ctype.hc` — ASCII classification (`Is*`/`ToUpper`/`ToLower`, 0/1).
- `fmt.hc` — printf-family intrinsic prototypes. `stdio.hc` is **gone**: `StdWrite`
  (portable stdout/stderr sink) + `STDOUT`/`STDERR` now live in `io.hc`. `printf.hc` —
  private printf core (`Pf` sink, `VFmt`), `#include`s `<io.hc>` for `StdWrite`.
  `fltfmt.hc` — **private** correctly-rounded float formatter `FmtFloat` (base-2³²
  bignum `Fbn`); auto-included; matches `fmt.rs` byte-for-byte.
- `bignum.hc` — `class Bn` minimal nonnegative big integer. `strconv.hc` — a
  compatibility shim that re-`#include`s `<cstr.hc>` (where `StrToF64`/`F64ToStr` now
  live).
- `bits.hc` → `math.hc` → `special.hc` (each includes the prior) — IEEE bit ops;
  elementary funcs (`Sqrt`/`Fabs`/rounding are optimization intrinsics; transcendentals
  are *defined* series, reproducible); special funcs (erf/gamma/Bessel). `rand.hc` —
  deterministic splitmix64 `RandU64`/`SeedRand`.
- `vec.hc` — `Vec<T>` owning growable typed array (scalar/pointer/class elements);
  `hmap.hc` — `Hmap<K,V>` hash map (separate chaining; hash/eq fn-ptrs; stock I64/string
  keys via private `strhash.hc` `Djb2`); `sort.hc` — generic `Sort<T>`/`BSearch<T>`
  (typed comparator `(T*,T*)`, median-of-three quicksort + typed swap; stock
  `CmpI64`/`CmpU64`/`CmpF64`).
- `io.hc` — fd I/O (`Open`/`LSeek`/`Read`/`Write`/`Close` primitives + `StdWrite` +
  `ReadFile`/`WriteFile`/…); flag `#define`s are canonical Linux values (Darwin/interp
  translate). `net.hc` — TCP (`Socket`/`Connect` + `TcpConnect`/`HttpGet`). `os.hc` —
  `Exit`/`Getpid`/…, `Remove`/`Rename`/`Mkdir`/`Chdir`/`Getcwd`, `Getenv`/`Environ` (pure
  HolyC over `EnvP`). `time.hc` — clock intrinsics + calendar math.
- `thread.hc` — `Thread`/`Join` (Darwin pthread; freestanding raw `clone(2)` + futex join;
  interp runs bodies synchronously at spawn). `sync.hc` — atomics (`Atomic*`,
  width-directed by the pointee) + `Mutex`/`Cond`/`RwLock` (Drepper futex locks in pure
  HolyC; freestanding globals are 16-byte-aligned since AArch64 exclusive / x86 `lock` ops
  fault on misalignment).

## HolyC / implementation semantics worth knowing
- Default int is `I64`; no `F32` (only `F64`). A bare string statement prints itself;
  `"fmt", a, b` is printf-style. A bare function name is a call (`Main;`).
- **Unknown calls are compile errors** (no implicit-extern fallback).
- **`public` visibility (directory-scoped):** a top-level function/`class`/`union`/global
  is visible to every file in the **same directory** unless declared `public`, in which
  case it is visible everywhere; a *cross-directory* reference to a non-`public` symbol is
  a compile error. Sema-only: `is_public` rides on `FuncDef`/`ClassDef`/`Declarator` (+
  `FuncSig`/`TypeDef`/`global_is_public` in sema); the gate is `check_visibility(is_public
  || same_dir(def, ref))`, where `same_dir` compares the two files' `FileInfo::dir` (the
  embedded stdlib is one dir, `<stdlib>`, so intra-stdlib helpers need no `public`; user
  code is a different dir, so it needs the stdlib API marked `public`). Compiler-generated
  code is trusted: `mono` stamps instances with the `GENERATED_FILE` sentinel span, and
  sema's `in_generated` flag / `ref_file == GENERATED_FILE` bypass the gate (so `Vec<Pt>`
  over a non-`public` `Pt` is fine); `#exe` output reuses the enclosing file's id.
  `typedef` aliases are exempt (parse-time, global). Most of `lib/*.hc`'s API is `public`;
  stdlib-internal helpers (e.g. `F64Bits`, the printf core) stay private and rely on the
  same-directory rule. A `public` function may not leak a non-`public` type through its
  **return type** (`check_public_signatures` peels pointers/arrays to the base named type;
  generated instances are skipped) — else an external caller couldn't name the result;
  it's a compile error. Tested in `tests/privacy.rs`. (This replaced the older `_`-prefix
  directory privacy; a leading `_` is now just a naming convention.)
- **Exceptions — `try`/`catch`/`throw` + `Fs`:** `throw expr;` raises a value (a bare
  `throw;` re-raises); `try { } catch { }` catches it (HolyC form, no catch parameter) and
  reads it as `Fs->except_ch`. `Fs` is the sema-injected implicit global `CTask *` (`CTask`
  defined in `lib/builtin.hc`). Interp (the oracle) unwinds via an `Err` carried by
  `pending_throw` (like `Exit`), with a `CTask` heap region whose `except_ch`/`catch_except`
  it reads/writes by byte offset; an uncaught throw finishes cleanly after the pre-throw
  output. **Native (arm64 + x86-64):** a jmp_buf/longjmp unwinder — each `try` builds an
  on-stack `ExcFrame` (prev, saved sp/fp, landing-pad addr, callee-saved set) pushed on
  the `Fs->exc_top` chain (`gen_try`); `throw` restores sp/fp/callee-saved from the top
  frame and indirect-branches to its landing pad (`gen_throw`) — so `gen_call` is
  untouched, zero per-call cost; uncaught → exit. arm64 additionally disables register
  promotion in `try` functions (`RegAnalysis::has_try`) so promoted locals aren't reverted
  by the restore; x86 never promotes, so locals are already in memory. **`Fs` storage is
  per-thread on every target with threads:** Darwin via pthread TLS (`gen_fs_ptr` reads a
  per-function cached slot filled by `emit_fs_cache` — `pthread_getspecific`/lazy-create +
  a key created in `_main`); freestanding arm64-linux via `TPIDR_EL0` (`msr` at `_start`
  for the main thread, in the `clone` child for spawned ones); freestanding x86-linux via
  the `%fs` base (`arch_prctl(ARCH_SET_FS)` at `_start`, the `clone` `CLONE_SETTLS` for
  children — `gen_fs_addr` reads `fs:[0]`, the `CTask` self-pointer). Windows uses the
  process-global `CTask` (`CTASK_GLOBAL`), which is correct since it has no threads. All
  freestanding setup is gated on `uses_exc`, so non-exception programs are byte-identical.
  The x86 unwinder is shared across both x86 OS targets, so the freestanding ELF and the
  Windows PE both compile `try`/`throw` (uncaught → the OS seam's exit: `exit_group` /
  `ExitProcess`). `examples/exceptions.hc` is a tracked example in `common::EXAMPLES`, so
  the per-example catch-all on **every** backend exercises it. Verified on
  aarch64-apple-darwin (`tests/arm64_darwin.rs`: native conformance + a per-thread pthread
  test); x86-64 linux/windows + freestanding-arm64 execution is verified on CI/docker (the
  per-example tests plus `tests/x86_64_linux.rs::exceptions_match_the_interpreter`) — only
  the emitter runs locally (no x86 runtime on macOS). Tested in `tests/interp.rs`,
  `tests/arm64_darwin.rs`, `tests/x86_64_linux.rs`, and the per-example suites.
- **`switch (x)` or `switch [x]`** (parsed identically); `start:`/`end:` sub-labels
  (keywords, so not usable as identifiers) are a prologue (runs on entry before dispatch)
  / epilogue (reached by fall-through, skipped by `break`). A dense, all-constant-case
  switch lowers to an O(1) jump table on arm64, else a compare-chain.
- **Chained range comparisons** (`a < b < c`, `0 <= i < n`) are a pure parser desugar to
  `(a<b) && (b<c)` (interior operands cloned, so `a < f() < b` calls twice); `==`/`!=`
  keep C's `(a==b)==c`.
- **String array initializers** desugar to byte brace lists: `U8 s[] = "abc"` → `s[4]` =
  `{'a','b','c',0}` (NUL appended then capped to N, so `U8 s[3]="abc"` drops it, matching
  C); a string into a *pointer* stays a pointer to the literal.
- **Locals are zero-initialized** (scalars and aggregates) in both backends, so reading an
  untouched local is defined.
- **Narrow ints (`U8`/`I8`/…/`U32`/`I32`) follow C width rules:** arithmetic promotes to
  `I64` (no mid-expression wrap), then truncates to the declared width at each boundary
  (store, arg-pass, return). `U8 f(){return 300;}` → `44`.
- **`>>`/`/`/`%` are signedness-directed** by the left operand's type (arithmetic vs
  logical shift; signed vs unsigned divide). **Relational `< > <= >=`** are
  signedness-directed by the usual arithmetic conversions (unsigned if either operand is);
  integer compares are full 64-bit, not via `f64`. **Float→int conversion is
  signedness-directed** (`FCVTZU`/`as u64` saturating vs `FCVTZS`/`as i64`). Interp keys
  off the target type in `cast_value`; native routes int-store sites through
  `gen_int_expr(e, target)`.
- **Scalar stores coerce to the lvalue type** in the interp (`coerce_to`): `I64 w = 3.14`
  → `3`, `F64 x = 5` → `5.0`, matching native register truncation/widening. `coerce_to`
  also decays a string literal into a pointer to one stable buffer (consistent pointer
  identity).
- **No `struct` keyword** — the aggregate is `class` + `union`, `repr(C)` (natural
  alignment, declaration order). A class passes/assigns **by value** (deep copy); arrays
  **decay to pointers**. An anonymous `union { … };` promotes its members into the
  enclosing class (`obj.field`); a named `union Name m;` is a member (`obj.m.field`).
- **Anonymous aggregate types + structural typing.** An anonymous `class { … }` /
  `union { … }` is a first-class type at any type position; the parser interns it to a
  synthetic `Named($Cls…/$ClsU…)` (deduped, signature-mangled via `anon_aggregate_name`,
  array dims folded into the mangle) and pushes a synthetic `ClassDef` to `pending_types`
  — like tuple/embedded-union synthesis, so layout/interp/backends are unchanged. Named
  classes stay **nominal** (so self-referential `class Node { Node *next; }` terminates in
  layout), but aggregates compare **structurally**: `sema::types_compatible` (a coinductive
  `compat` with a `seen` cycle guard) accepts two types with the same ordered
  `(field name, field type)` list + same kind (`is_union` on `TypeDef`), so same-signature
  named/anon/`typedef class{…}` types interchange in `check_assignable` (assign/return/init;
  args stay arity-only). Anon-in-generic-template referencing a type param is a parse error.

## The arm64/x86 ABI + shared formatting
Because solomon functions only call each other, the native backends use an internally
consistent ABI (not full AAPCS64/System V packing): int/ptr args in `x0–x7`/`rdi…`, F64 in
`v0–v7`/`xmm0…` (the two classes numbered independently); classes by value carried by
address (the callee copies; class returns via an sret pointer in `x8`/`r11`); array params
decay to a by-reference pointer; `&Func` is a self-resolved address and an indirect call
classes its args off the callee's `Type::FuncPtr`. Print formatting is shared via
`src/fmt.rs`: it parses each `%[flags][width][.prec]conv` spec once; the interpreter
renders the value (`render_int`/`render_str`/`render_exp`/`render_g`), the backends compile
the pure-HolyC `FmtFloat` for floats — byte-identical.

## Generics (monomorphized; `src/mono.rs`)
The parser never instantiates; it **defers** every generic use to an AST node — `Vec<I64>`
→ `Type::Generic`, `(I64,F64)` → `Type::Tuple`, `Id<I64>(x)`/`Id(x)` →
`ExprKind::GenericCall`, `:=` → `StmtKind::ShortDecl`, `switch type(T)` →
`StmtKind::TypeSwitch` — and carries the parsed-once templates on `Program::generics`.
`mono::expand` resolves all of it to a **fully concrete** program (no deferred node
remains; sema/layout/interp/backends keep `unreachable!` arms as assertions). Two-pass
(signatures then bodies) + a worklist drained to a fixpoint, deduped by an injective
mangled name. Instantiation is **AST substitution** (`subst_type`/`subst_expr`/
`subst_stmt`), no token re-parse; the type-arg grammar parses recursively (`>>` split by
`expect_generic_gt`).

**Inference** (`Mono::infer` + `type_expr`, a real scoped recursive typer over the whole
parsed program): unify each parameter's template type against the argument's static type.
Drives inferred calls (`VecPush(&v, x)`) and `:=` (`n := e`; `a, b := e` unpacks a tuple).

**Parameter kinds** (`ast::GenericParam`): every generic parameter **must** carry a kind
keyword — a bare `<T>` is a parse error (`parse_generic_param` / the captured-fn param
parser both reject it). `type T` is an unconstrained type parameter (the stdlib generics
are written this way, e.g. `class Vec<type T>`, `U0 VecPush<type T>(…)`, and a `public`
generic fn whose return type is a type param works);
`comparable T` (constrained to a `<`/`>`-orderable type — enforced at instantiation
via `sema::is_scalar`, so a class argument is a clean error), and `int N` (a compile-time
**value parameter**, e.g. an array size `T data[N]`). Use-site args are `ast::GenericArg`
(`Type`/`Value`). mono substitutes a value param to a concrete `Int` everywhere (array dims
+ expressions), preserving the fully-concrete invariant; value args are explicit-only;
mangling folds a value as `C<n>E` (`FixedArr<I64,8>` → `8FixedArrI64C8E`). **`switch type
(T) { case I64: … default: … }`** is a compile-time type switch: mono resolves the
scrutinee's type, keeps only the matching arm (else `default`, else nothing), and
**discards the rest before sema** — so a dead arm ill-typed for the chosen `T` never
errors, and the surviving arm sees the concrete type. The `type` keyword does double duty
(param introducer + switch marker). Stdlib `Vec<T>`/`Hmap<K,V>`/`Sort<T>` are generic; see
`examples/generic.hc`.

## Status / examples
The backends compile the whole implemented subset: the `offset` keyword, brace +
designated initializers (nested/partial/out-of-order, arrays of classes — a shared
`gen_init_into` per backend), member access on a call result (`Mk().x`), function pointers
end to end (`&Func`, calls through a pointer, fn-ptr class fields/arrays/vtables), and
`typedef` aliases (incl. function-pointer and anonymous-aggregate aliases
`typedef class{…} Name`, resolved at parse time, defined before use). `#exe { … }` runs
HolyC at compile time via the interpreter and splices its stdout.
**Still absent:** most of the TempleOS core/standard library and DolDoc.

Worked programs live in `examples/*.hc`, listed once in `tests/common.rs`
(`common::EXAMPLES`) and exercised by `tests/examples.rs` (tokenize/parse/sema/run-clean),
`tests/programs.rs` (exact interpreter output for the substantive ones), and
`tests/arm64_darwin.rs::native_matches_interp_for_every_example` (compiles every example
natively and asserts byte-for-byte equality with the interpreter — the catch-all that keeps
new examples and backend changes honest).
