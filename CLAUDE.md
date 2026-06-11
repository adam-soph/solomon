# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

solomon is a from-scratch reimplementation of **HolyC** (Terry Davis's TempleOS
language) in Rust: a real compiler front end that lowers to an **SSA IR**
(`src/ir.rs`), an IR **interpreter** (`src/irinterp.rs`, the **conformance oracle**),
and hand-rolled native code generators behind the **`Codegen`** trait
(`src/backend.rs`). A backend is an (arch, OS) pair, not just a CPU:
`aarch64-apple-darwin` (Mach-O via `cc`), `aarch64-unknown-linux` and
`x86_64-unknown-linux` (**freestanding** static ELFs — no libc, no linker, raw
syscalls), and `x86_64-pc-windows` (a self-contained PE with hand-built `kernel32`
imports). Darwin is the one hosted target. Every backend matches the IR interpreter
byte-for-byte on all examples.

**Both backends consume the IR**: `src/backend/arm64/isel.rs` and `src/backend/x86_64/isel.rs` are
machine-code generators (instruction selectors) that walk the *one* SSA IR — they are
**not** separate IRs. The old AST-walking x86 `Cg` is gone (`src/backend/x86_64/mod.rs` now holds
only the `OsTarget` seam + `Asm` register numbering). `src/backend.rs` no longer holds the
old AST `Emitter` drivers; it is now the **whole native-codegen layer**: the outer
`Codegen` trait + `CodegenError` (the small `src/codegen.rs` was folded in here — one
module, not two) on top of the **IR-level shared driver** — the pure-IR analyses
(`reachable_functions`/`heap_prims_used`/`func_uses_fs`), the `Backend` trait +
`emit_blocks` block-walk loop both `isel`s share, and the arch-neutral **out-of-SSA +
register-promotion** pass (`destruct_program`/`plan_registers`, folded in from the former
`src/regalloc.rs`) — so they can't drift on any of it. The original
tree-walking AST interpreter is also gone, and so is the `src/interp.rs` shim that
outlived it: its OS-syscall free-helpers, `FdObj`, and the `run_to_string`/`run_to_bytes`
entry points (which lower → run the IR interpreter) now live in `src/irinterp.rs`
alongside the engine that consumes them. The whole compiler is one shared SSA middle-end
feeding the interpreter and both code generators.

## Commands

```sh
cargo build                 # debug (CLI: target/debug/hcc; the lib crate is `hcc`)
cargo build --release
cargo test                  # whole suite (unit tests in src/ + the tests/ crates)
cargo test --lib            # just the in-src unit tests (each module's *_tests.rs sibling)
cargo test --test integration    # the ~940 .hc cases under tests/cases/ (native vs oracle)
SOLOMON_SKIP_NATIVE=1 cargo test --test integration   # fast lane: oracle + structural only
cargo test strings_hello    # a single test by name substring
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

### Test layout
Two homes, by kind:
- **Unit tests live in `src/`**, each module beside its code in a `*_tests.rs` sibling
  wired with `#[cfg(test)] #[path = "foo_tests.rs"] mod tests;` (e.g.
  `src/frontend/{lexer,parser,preproc,sema,layout}_tests.rs`,
  `src/{ir,backend,main}_tests.rs`, and `src/irinterp_tests.rs` +
  `src/irinterp_conformance_tests.rs`). `cargo test --lib`/`--bin hcc` runs them.
- **`tests/` holds integration + comparison + conformance crates:**
  - `tests/cases/**/*.hc` — ~940 HolyC programs, **one `#[test]` per file**, discovered at
    compile time by the `hcc_test_macros::test_case!("cases")` proc-macro (it globs that
    directory and emits the tests; the crate lives at `tests/macros/`). A **three-way agreement
    check**: the interpreter, the host-native binary, and a committed expected output
    (`<case>.out` beside the `.hc`) must all produce the same bytes (`native == interp ==
    expected`). The committed golden is the third anchor — a `native == interp` check alone is
    blind to a bug both engines *share* (e.g. a `FmtFloat` change shifts both at once); the
    frozen golden turns that into a caught regression. A host-independent structural check
    validates the emitted object; native execution self-skips off a runnable host (the matching
    CI leg covers it), leaving `interp == golden` + structural there. Regenerate the goldens
    with `SOLOMON_BLESS=1 cargo test --test integration` (writes the interpreter output to each
    `.out`); **review the diff before committing**. A leading `//@ args:` / `//@ stdin:` comment
    supplies argv/stdin (parsed by `tests/support/mod.rs::run_case`). Note: the proc-macro globs
    at compile time, so after **adding/removing** a `.hc` (or a `tests/bench/<name>` pair) force a
    re-glob — `touch tests/integration.rs` (or `cargo clean -p hcc`) — before it appears.
  - `tests/bench/<name>/{prog.hc,prog.c}` + `tests/comparison.rs` — HolyC-vs-C pairs:
    assert identical stdout, then time both and warn (never fail) if HolyC is >5x slower.
  - `tests/structural.rs` — emits **all four** backends' images and byte-checks the
    container (Mach-O/ELF/PE) on **every** host, plus the predefined-target-macro tests.
  - `tests/visibility.rs` — cross-directory `public` visibility (multi-file/temp-dir).
  - `tests/stdlib.rs` — stdlib conformance, also three-way (`native == interp == expected`)
    but with an **independent Rust expected** (`format!("{:e}", …)` for the float formatter,
    `str::parse` for the parsers), so it catches a *current* shared bug the golden snapshots
    can't (integration's blessed goldens freeze whatever the formatter does today). The
    `run_with_stdlib` helper runs both engines; the test asserts the Rust oracle. (`MSize` is
    interp-only there: hosted Darwin maps `MAlloc`→libc `malloc`, so native `MSize` returns 0 —
    a documented limitation, not a parity bug.)
  - `tests/{io,net,thread,sync}.rs` — impure-primitive property tests.
- **Don't run docker tests locally.** The impure-feature crates (`io`/`net`/`thread`/`sync`)
  and the freestanding ELF *execution* run under docker on CI and self-skip locally; native
  execution in `tests/integration.rs` self-skips off a runnable host (structural still runs).
  A green `cargo test` on an Apple-silicon Mac exercises every *emitter* (structural) and the
  AArch64 Mach-O *execution* lane, but not Linux/x86 *execution* — verify those on CI.
- **The bar is warning-free `cargo build`** plus fmt-clean. Clippy reports pre-existing
  unenforced lints; don't fix the tree, just don't add new warnings.

## Architecture

### Pipeline
`lexer → preprocessor → parser → mono → sema → layout → lower (AST→SSA IR) →
{ irinterp | arm64 backend | x86 backend }`, each a module. The six front-end passes
(lexer/preproc/parser/mono/sema/layout) are grouped under **`src/frontend/`** (mirroring
how the backends are grouped under `src/backend/arm64/`·`src/backend/x86_64/`); they are also re-exported
at the crate root, so `crate::parser::…`/`hcc::sema::…` resolve unchanged. The shared
data types they produce — `src/ast.rs` and `src/token.rs` — stay at the crate root, since
the IR, interpreter, and backends consume them too. `parser::parse(src)` is the
entry point and is **two-pass**:
`hoist_type_names` streams
the tokens to collect `class`/`union` names (so a type can be used before it's defined),
then the real parse, then the **`mono`** pass (`src/frontend/mono.rs`) which monomorphizes every
deferred generic into concrete AST — so everything downstream sees an ordinary,
**fully-concrete** `Program`. `sema::check_program` does name resolution + type
inference; `layout::compute` does `repr(C)` sizes/offsets; `src/main.rs` wires it per
mode. Entry points: `parse(src)` (CWD base, embedded stdlib only), `parse_in_dir`,
`parse_with(src, dir, search)`; the CLI passes the input file's parent +
`hcc::stdlib_dirs()` (the `SOLOMON_STDLIB` overrides; `-I DIR` prepends more).

**Float-formatter auto-include.** Float printing is a pure-HolyC function (`FmtFloat`,
folded into `<stdio.hc>` next to the printf core) that *every* target runs, not emits;
with no dead-code elimination it can't be linked unconditionally, so it rides in on the
same `<stdio.hc>` auto-include any print triggers (`hoist_type_names` sets `uses_print`;
the prelude prepends `#include <stdio.hc>`, guard-deduped). The interpreter and the
freestanding/x86/Windows backends compile-and-run the one `FmtFloat` body; Darwin alone
links the same body but defers the irreducible leaf to libc. There is no separate Rust
formatter — the printf family is one HolyC implementation on every target.

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

### The SSA IR (`src/ir.rs`, `src/frontend/lower/`; out-of-SSA in `src/backend.rs`)
The front end lowers to a typed **SSA IR** (`src/ir.rs`): typed virtual registers
(`IrTy` widths + signedness baked into ops), basic blocks with `phi`, explicit
`alloca`/`load`/`store`, `getelementptr`-style `PtrAdd`, near-machine ops, and
`Call`/`Prim`/`TryBegin`/`TryEnd`/`Throw`. Everything tricky (narrow-int promote/truncate,
`>>`/`/`/`%`/relational signedness, float↔int conversion, store/arg/return coercion) is
**decided once during lowering** from `e.ty()` and frozen into the ops, so the interp and
the backend can't re-derive it differently.

- **`src/frontend/lower/`** — AST→SSA, built **on the fly** during the AST walk (Braun et al.
  "Simple and Efficient SSA Construction"; `read_variable`/`write_variable` over
  sealed/unsealed blocks, no dominance frontiers). GCC-style **register/memory split**:
  a non-address-taken scalar becomes a `phi`/vreg SSA value; everything address-taken or
  aggregate gets an `alloca` slot reached via `load`/`store`. The synthesised `@entry`
  function holds the top-level code. Const-folds here. `lower(program, layouts)` is the
  entry; `intrinsics::is_primitive` names map to `Prim`. Split into submodules: `mod.rs`
  (the driver + shared helpers like `coerce`), `expr.rs` (expressions), `stmt.rs`
  (statements + control flow), `init.rs` (brace/designated initializers), and `ssa.rs`
  (the `read_variable`/`write_variable` SSA construction core).
- **Out-of-SSA + register promotion (`src/backend.rs`)** — folded in from the former
  `src/regalloc.rs`, since it consumes the IR like the rest of the shared backend support
  and the interpreter skips it. `destruct_ssa`/`destruct_program` resolves `phi`s into
  copies on CFG edges (critical-edge splitting, parallel-copy sequencing). Also
  **`plan_registers`**, a liveness-based linear-scan **register promotion** pass: it lifts
  hot vregs (≥2 refs) into the target's callee-saved registers — pools passed in by each
  backend (arm64 x19–x28/d8–d15; x86-64 rbx/r12–r14, no float pool) — returning
  `vreg → Option<PReg>`. It is additive over the backends' spill-everything model (an
  unpromoted vreg emits exactly as before); a `try`-containing function is left fully
  spilled (a `throw`'s longjmp would not restore callee-saved registers).

**x86 consumes the IR too** (`src/backend/x86_64/isel.rs`). It walks the phi-free IR and emits
x86-64, reusing the `Asm` encoder and the **`OsTarget` seam** in `src/backend/x86_64/mod.rs`
(per-OS deltas: exit, page alloc, std write, file ops, clock, command-line capture;
freestanding ELF vs Windows PE). Spill-everything **+ promotion** (both OSes): a vreg lives
in an `[rbp-off]` slot unless `backend::plan_registers` lifts it into a callee-saved GPR
(**rbx/r12–r14**, saved/restored in prologue/`teardown`; r15 excluded — the Windows seam
uses it; no float promotion, since System V has no callee-saved xmm). On Windows a
**frame larger than one page is stack-probed** in the prologue (`Asm::prologue_probe`:
commit each 4 KiB page descending to `rsp-frame`) — the PE commits only one stack page and
we emit no `__chkstk`, so a deep access (e.g. a promoted vreg's prologue spill at the
bottom of the frame) would otherwise skip the guard page and fault. Scratch rax/rcx/rdx +
rsi/rdi (all low regs, so the parametric `load_local_reg`/`store_local_reg`/`lea_local_reg`
need no REX.R) and xmm0/xmm1; the internal ABI matches arm64 (int args rdi/rsi/rdx/rcx/r8/r9,
F64 xmm0–7, sret pointer in **r11**); single-task `Fs` as a BSS `CTask` seeded in `@entry`,
gated on real Fs/exception use (`func_uses_fs`), so non-exception programs stay BSS-lean; a
32-byte `ExcFrame`; `Sqrt`→`sqrtsd`/`Fabs`→`andpd`; `clone(2)` threads with base in rbx. The
old AST `Cg` is gone — `mod.rs` keeps only the `OsTarget` trait, register consts, and
`align16`/`load_opcode`/`store_opcode` (used by `asm.rs`); the block-walk + pure-IR scans
are shared via `src/backend.rs` (`reachable_functions`/`heap_prims_used`/`func_uses_fs` +
the `Backend` trait/`emit_blocks`).

### Interpreter & backends
- **`irinterp.rs`** — the **conformance oracle**: a flat-byte-addressable IR interpreter.
  `IrInterp` runs over a `Mem` of three real-address regions (stack/data/heap;
  `DATA_BASE`/`HEAP_BASE`/`FUNC_BASE`); SSA vregs are an `RVal` (Int/Float) register file;
  pointers are real `u64` addresses, so ptr↔int casts round-trip, byte-indexing through
  `&scalar` works, and union/type-punning is just overlapping bytes — the old tree-walk's
  `Value`/`Region`/`Place`/`PtrTable` limitations are gone. It implements the **full
  impure-primitive set** (clock, fd I/O, sockets, fs mutation, process ids, threads run
  **synchronously** at spawn, atomics, futex) over `std`, plus real argv/env/stdin and
  `Exit`. Its own `run_to_string`/`run_to_bytes` entry points lower → run it; the CLI
  `-i` path does the same. **Match its observable output when adding backend features.**
- **`arm64/isel.rs`** (the IR walk + per-function `FnEmit`, with submodules under
  `isel/`: `prims.rs` — OS-primitive selection that `emit_prim` dispatches to
  (heap/clock/thread/atomic/file/process); `heap.rs` — the freestanding `mmap`
  bump-allocator runtime; `exc.rs` — the `try`/`throw` unwinder. Plus
  `asm.rs`/`darwin.rs`/`linux.rs`.) — walks the `phi`-free IR
  (after `backend::destruct_program`) and emits AArch64: a Mach-O object linked with `cc`
  (Darwin), or a freestanding static ELF (`aarch64-unknown-linux`, via the `ArmTarget`
  seam: own `_start`, raw syscalls, `mmap` bump allocator). **Spill-everything + promotion**:
  a vreg lives in a frame slot unless `backend::plan_registers` lifts it into a callee-saved
  register; one `Ctx` selects hosted-Darwin (common-symbol globals, libc primitives) vs
  freestanding (BSS-offset globals reached by self-resolved `ADR`, syscall primitives).
  Reuses the `Asm` encoder + its post-emission **peephole** (`Asm::peephole`). A dense,
  all-constant `switch` lowers to an O(1) jump table (`try_switch_table`), else a
  compare-chain; the algebraic intrinsics `Sqrt`/`Fabs`/the rounding family lower to single
  FP instructions (`try_intrinsic`) in place of their lib bodies. (Still not reimplemented
  from the deleted AST backend: `try_imm_binop` immediate-form strength reduction.)
- **`x86_64/isel.rs`** (the IR walk + `FnEmit`, with submodules under `isel/`: `prims.rs`
  — OS-primitive selection plus the Win32 `emit_win_call` import marshalling; `heap.rs` —
  the freestanding bump-allocator runtime (page-alloc via the `OsTarget` seam); `exc.rs` —
  the `try`/`throw` unwinder. Plus `linux.rs`/`windows.rs`.) — walks the phi-free IR and emits
  x86-64 (default), to a freestanding static ELF (`x86_64-unknown-linux`) or, via the
  `OsTarget` seam, a self-contained PE with hand-built kernel32 imports
  (`x86_64-pc-windows`). Spill-everything in `[rbp-off]` slots + `plan_registers` promotion
  into rbx/r12–r14 (both OSes; Windows stack-probes frames >1 page, since the PE commits one
  page and emits no `__chkstk`); rax/rcx/rdx + rsi/rdi scratch, xmm0/xmm1 F64; System V-style
  internal ABI; compare-chain `switch`. **`mod.rs`** now holds only the shared `OsTarget` seam
  + register numbering (no AST `Cg`).

Both backends cover the whole implemented subset; only the deliberately-excluded
transcendentals are absent (they're lib functions, below).

### intrinsics (the one compiler-provided-behaviour seam)
`intrinsics.rs` is the single seam: a **standard-library function declared in `lib/*.hc`**
(resolved like any call, *with* an `#include`) that the backends lower specially. (The old
`builtins.rs` registry is **deleted**.) Two flavours (`IntrinsicKind`):
- **Optimization** — has a real HolyC body a backend may replace with an instruction where
  supported, else calls the body (`Sqrt` → `fsqrt`/`sqrtsd`, else the lib Newton; both
  correctly rounded, so they agree). The interpreter always runs the body.
- **Primitive** — a body-less lib *prototype* the backend *must* lower (it bundles an OS
  syscall): the `StdWrite` sink, the heap (`MAlloc`/`Free`/`HeapExtend`/`MSize`), the clock,
  fd I/O + sockets, fs mutation + process control, threads, atomics. The **printf family is
  not here** — `Print`/`StrPrint`/`CatPrint`/`MStrPrint` and the `VFmt`/`FmtFloat` core are
  pure HolyC in `<stdio.hc>` (bottoming out at `StdWrite`), so they are ordinary functions
  every target compiles and runs.

Dispatch (lowering → `Prim`; both IR backends' `emit_prim`, the IR interp's `exec_prim`)
gates on `intrinsics::is_primitive(name)`. A **compiled user
function shadows a like-named primitive** (a program's own `Read`/`Join`) — a body in
`funcs` means "call the body." A bare string statement prints verbatim (a direct
`StdWrite`, no `%` processing); the `"fmt", args` comma form lowers to a `Print` call, so
it needs `<stdio.hc>` — which the print auto-include always supplies.

The only compiler-provided names with **no `lib/*.hc` declaration** are the implicit
`argc`/`argv`, the environment `envp`, and the exception task `Fs` — sema-injected
(doc-commented in `lib/builtin.hc`), captured at entry. `argc`/`argv` are **scope-dual but
not global**: at **top-level scope** they are the command line; inside a `...` function
they are the variadic args (count + an `I64 *` of raw 8-byte slots); inside a non-variadic
function they are *undeclared* (sema resolves the top-level case in `check_ident` only when
`cur_ret.is_none()`, and the varargs case via locals it declares in `...` functions). The
command-line capture is gated on top-level `argc`/`argv` use (`ast::program_uses_command_line`
skips all function bodies), so a `printf` caller — which uses them only as varargs — never
drags in the command line. `envp` (single meaning) is by contrast a plain global, in scope
everywhere (e.g. `Getenv` walks it).
(On hosted Darwin, `emit_prim` maps the heap primitives to libc — `MAlloc`→`_malloc`,
`Free`→`_free`; freestanding emits an `mmap` bump-allocator runtime.) Everything reducible
is pure HolyC in `lib/*.hc`, so
each function computes identically on every target; each lib file has an `#ifndef _NAME_HC`
guard. The **impure groups** (clock, fd I/O, sockets, fs mutation, process ids, threads)
are conformance-tested by *property* (e.g. monotonic clock, write→read round-trip), never
interp-vs-native value.

### Standard library (`lib/*.hc`)
The stdlib mirrors **C/POSIX headers** — filenames and groupings follow `<string.h>`,
`<stdio.h>`, `<stdlib.h>`, etc., though the function names stay HolyC-PascalCase
(`StrLen`, `MAlloc`, `Print`). Each public module is includable on its own; the
human-readable header in each file is the API reference. Map:

Public C-named headers:
- `builtin.hc` — implicit prelude (no `#include`): `NULL`/`TRUE`/`FALSE`, `MAlloc`/`Free`
  prototypes, `CTask`, doc for the sema-injected `argc`/`argv` (command line / varargs),
  `envp`, and `Fs`.
- `string.hc` (`<string.h>`) — C `U8 *` string ops (`Str*` family) **and** the raw-memory
  `mem*` family (`MemCpy`/`MemMove`/`MemSet`/`MemCmp`/`MemFind`/`MemSearch`), plus `CmpStr`.
- `ctype.hc` (`<ctype.h>`) — ASCII classification (`Is*`/`ToUpper`/`ToLower`, 0/1).
- `stdio.hc` (`<stdio.h>`) — printf family (`Print`/`StrPrint`/`CatPrint`/`MStrPrint`),
  `Remove`/`Rename`, and path file helpers (`AppendFile`/`FileSize`).
  Holds the private printf core (`Pf` sink, `VFmt`) and the correctly-rounded float
  formatter `FmtFloat` (base-2³² bignum `Fbn`, private `FltBits` punning so it needn't pull
  `<math.hc>`; cross-checked against a Rust `%e`/`%g` oracle in `tests/stdlib.rs`).
  `#include`s `<string.hc>`/`<fcntl.hc>`/
  `<unistd.hc>` only — `MStrPrint` grows via `MAlloc`+`MemCpy`+`Free`, so a plain printing
  program stays lean. Auto-included when a program prints.
- `stdlib.hc` (`<stdlib.h>`) — `CAlloc`/`ReAlloc`/`MSize`/`HeapExtend`; the number<->string
  conversions `StrToI64`/`I64ToStr`/`StrToF64`/`F64ToStr` (`StrToF64` is the
  correctly-rounded `atof` over the private `Bn` big integer; `F64ToStr` its
  shortest-round-trip inverse via `<stdio.hc>`'s `FmtFloat`, the one outward dependency);
  `Sort`/`BSearch` (`qsort`/`bsearch`); `RandU64`/`SeedRand`; `Exit`; `Getenv`.
  Auto-included when a program uses `F64ToStr`.
- `math.hc` (`<math.h>`) — opens with the IEEE bit access/classification (`Float64bits`/
  `IsNaN`/`Signbit`/`Copysign`/`NaN`/`Inf`), then the elementary funcs (`Sqrt`/`Fabs`/
  rounding are optimization intrinsics; transcendentals are *defined* series,
  reproducible), then the special funcs (erf/gamma/Bessel). Standalone (no includes).
- `time.hc` (`<time.h>`) — clock intrinsics + calendar math.
- `fcntl.hc` (`<fcntl.h>`) — `Open` + the `O_*`/`MODE_0644` flags. `unistd.hc`
  (`<unistd.h>`) — `Read`/`Write`/`Close`/`LSeek`/`StdWrite`/`WriteAll` + `SEEK_*` +
  `STDOUT`/`STDERR`; `Getpid`/`Getppid`/`Getuid`/`Getgid`/`Chdir`/`Getcwd`/`Mkdir`. Flag
  `#define`s are canonical Linux values (Darwin/interp translate).
- `socket.hc` (`<sys/socket.h>`) — TCP (`Socket`/`Connect` + `ParseIPv4`/`MakeSockaddr`).
- `threads.hc` (`<threads.h>`) — `Thread`/`Join` (Darwin pthread; freestanding raw
  `clone(2)` + futex join; interp runs bodies synchronously at spawn) **and**
  `Mutex`/`Cond`/`RwLock` (Drepper futex locks in pure HolyC). `stdatomic.hc`
  (`<stdatomic.h>`) — atomics (`Atomic*`, width-directed by the pointee) + `AtomicFence` +
  `FutexWait`/`FutexWake`. Freestanding sync globals are 16-byte-aligned (AArch64 exclusive
  / x86 `lock` ops fault on misalignment).

Container extensions (no C equivalent):
- `vec.hc` — `Vec<T>` owning growable typed array (scalar/pointer/class elements); also
  hosts `Environ(Vec<U8*>*)` since it builds a `Vec`. `hmap.hc` — `Hmap<K,V>` hash map
  (separate chaining; hash/eq fn-ptrs; stock I64/string keys via a private `Djb2` string
  hash defined in the file).

Every other module's internals were folded into the C header that owns them — the lib
directory is only C-named headers plus the two container extensions. `qsort`/`bsearch`/the
`Bn` big integer/the heap helpers fold into `<stdlib.hc>`; the printf core + `FmtFloat`
into `<stdio.hc>`; the IEEE bit ops into `<math.hc>`; the djb2 hash into `<hmap.hc>`.

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
  it's a compile error. Tested in `tests/visibility.rs`. (This replaced the older `_`-prefix
  directory privacy; a leading `_` is now just a naming convention.)
- **Exceptions — `try`/`catch`/`throw` + `Fs`:** `throw expr;` raises a value (a bare
  `throw;` re-raises); `try { } catch { }` catches it (HolyC form, no catch parameter) and
  reads it as `Fs->except_ch`. `Fs` is the sema-injected implicit global `CTask *` (`CTask`
  defined in `lib/builtin.hc`). **IR interp (the oracle)** unwinds via a per-frame
  try-region stack: `TryBegin` pushes its landing-pad block, a `Throw`/`Rethrow` or a
  `Call` that returns `Outcome::Threw` pops to the nearest pad; `except_ch`/`catch_except`
  are byte writes into the `CTask`; an uncaught throw finishes cleanly after the pre-throw
  output. **Native** is a jmp_buf/longjmp unwinder: each `try` builds an on-stack
  `ExcFrame` pushed on the `Fs->exc_top` chain; `throw` restores sp/fp from the top frame
  and indirect-branches to its landing pad — the normal call path is untouched, zero
  per-call cost; uncaught → exit. **arm64 (`isel/exc.rs`):** `TryBegin`/`TryEnd`/`Throw`/`Rethrow`; the
  `ExcFrame` is just `{prev, saved_sp, saved_fp, landing_pad}` (32 bytes, **no
  callee-saved set** — spill-everything keeps nothing in callee-saved registers). `Fs` is
  **per-thread** on Darwin via pthread TLS: an `Fs`-using function caches this thread's
  `CTask*` in a frame slot filled in the prologue (`emit_fs_cache` — `pthread_getspecific`
  / lazy-`malloc` + `pthread_setspecific`, key created in `@entry`), and `&Fs` resolves to
  that slot. Freestanding arm64 spawns real `clone(2)` threads but keeps a single BSS
  `CTask`, so concurrent cross-thread `throw`s race (non-exception parallelism is fine).
  **x86-64 (`isel/exc.rs`):** `emit_try_begin`/`emit_try_end`/`emit_unwind` with the same
  32-byte `ExcFrame` (no callee-saved set — spill-everything); `Fs` is a single BSS `CTask`
  seeded into the `Fs` global at `@entry`, so concurrent cross-thread `throw`s race like
  freestanding arm64. All setup is gated on real `Fs`/exception use (`func_uses_fs`).
  `examples/exceptions.hc` is ported into `tests/cases/ported_examples/`, so the
  integration suite's native-vs-oracle comparison exercises it on every backend, and the
  `tests/cases/exceptions/` corpus covers the rest. Verified on aarch64-apple-darwin
  (`tests/integration.rs` native lane); x86-64 + freestanding-arm64 execution on CI. Pinned
  interp behaviour lives in `src/irinterp_conformance_tests.rs`.
- **`switch (x)` or `switch [x]`** (parsed identically); `start:`/`end:` sub-labels
  (keywords, so not usable as identifiers) are a prologue (runs on entry before dispatch)
  / epilogue (reached by fall-through, skipped by `break`). Lowered to a `Switch`
  terminator (keeping `(lo,hi)` case ranges); arm64 lowers a dense, all-constant switch to
  an O(1) jump table (`try_switch_table`), else a compare-chain; x86 always compare-chains.
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
  signedness-directed** (`FCVTZU`/`as u64` saturating vs `FCVTZS`/`as i64`). **All of this
  is frozen at lowering** into the IR ops' `signed`/`ty` fields and `Cast{from,to}`, so the
  IR interp and the backend cannot re-derive it differently; the IR interp's `bin`/`cast`
  mirror the same rules.
- **Scalar stores coerce to the lvalue type**, decided at lowering (`coerce`/
  `coerce_to_ast` in `lower`, emitting a `Cast`): `I64 w = 3.14` → `3`, `F64 x = 5`
  → `5.0`, matching native register truncation/widening. A string literal in value
  position decays to a stable interned-pointer address.
- **No `struct` keyword** — the aggregate is `class` + `union`, `repr(C)` (natural
  alignment, declaration order). A class passes/assigns **by value** (deep copy); arrays
  **decay to pointers**. An anonymous `union { … };` promotes its members into the
  enclosing class (`obj.field`); a named `union Name m;` is a member (`obj.m.field`).
- **Anonymous aggregate types; nominal typing.** An anonymous `class { … }` /
  `union { … }` is a first-class type at any type position; the parser interns it to a
  synthetic `Named($Cls…/$ClsU…)` (deduped, signature-mangled via `anon_aggregate_name`,
  array dims folded into the mangle) and pushes a synthetic `ClassDef` to `pending_types`
  — like tuple/embedded-union synthesis, so layout/interp/backends are unchanged.
  Aggregates are **nominal**, matching HolyC: `sema::types_compatible`/`compat` accepts two
  `class`/`union` types only when they are the *same* named type, so two differently-named
  same-fielded classes never assign across each other (reinterpret via a pointer cast,
  `union`, or `MemCpy` instead). Identical anonymous/tuple types share one synthetic name,
  so they still match via the `a == b` fast path. `check_assignable` gates assign/return/
  init, and `check_arg` checks **aggregate** call arguments the same way (a `class`/`union`
  value must match its parameter's named type); scalar and pointer arguments stay permissive
  (int/float/pointer conversions, `NULL`, array decay, and pointer reinterpretation).
  Anon-in-generic-template referencing a type param is a parse error.

## The arm64/x86 ABI + shared formatting
Because solomon functions only call each other, the native backends use an internally
consistent ABI (not full AAPCS64/System V packing): int/ptr args in `x0–x7`/`rdi…`, F64 in
`v0–v7`/`xmm0…` (the two classes numbered independently); classes by value carried by
address (the callee copies; class returns via an sret pointer in `x8`/`r11`); array params
decay to a by-reference pointer; `&Func` is a self-resolved address and an indirect call
classes its args off the callee's `Type::FuncPtr`. Print formatting is **one HolyC
implementation** shared by every target: the printf family (`Print`/`StrPrint`/… → the
`VFmt` spec parser → `FmtFloat`) lives in `<stdio.hc>`, so the IR interp runs those bodies
and the backends compile-and-call them — no Rust formatter, byte-identical by
construction. A bare string prints verbatim (a direct `StdWrite`); the `"fmt", …` comma
form lowers to a `Print` call in `lower`.

## Generics (monomorphized; `src/frontend/mono.rs`)
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
errors, and the surviving arm sees the concrete type. **`if type (T is U) … [else …]`**
(`is not` negates) is the single-case analogue: a pure parser desugar (`parse_type_if`) to
a one-arm `TypeSwitch`, so it shares all the mono machinery — both sides are types (usually
a type param on the left); `is`/`not` are contextual words, not reserved. Used by
`lib/math.hc`'s `Min`/`Max`/`Abs` to add the float-only path (return `T`, with `fmin`/
`fmax` NaN handling / `Fabs`). The `type` keyword
does double duty (param introducer + switch/if marker). Stdlib `Vec<T>`/`Hmap<K,V>`/
`Sort<T>` are generic; see `examples/generic.hc`.

## Status / examples
The backends compile the whole implemented subset: the `offset` keyword, brace +
designated initializers (nested/partial/out-of-order, arrays of classes — lowered to
`MemZero` + leaf `Store`s in `lower`), member access on a call result (`Mk().x`),
function pointers
end to end (`&Func`, calls through a pointer, fn-ptr class fields/arrays/vtables), and
`typedef` aliases for scalars, pointers, classes, and the anonymous-aggregate form
`typedef class{…} Name` (resolved at parse time, defined before use). A **function-pointer
type** is named two ways, both putting the name *outside* the declarator:
`typedef I64 (*)(I64) Name;` (an anonymous fn-ptr type with the name after it — the
consistent `typedef <type> <name>` shape) or the keyword-less bare declarator
`I64 (*Name)(I64);` (a top-level declarator with no initializer; an initializer makes it an
ordinary global, and the same shape at local scope stays a variable — `parser.rs`'s
`top_level` flag gates this). The **C-style `typedef I64 (*Name)(I64);`, with the name
buried inside the declarator, is a compile error** (`parse_typedef` rejects a named
`Type::FuncPtr`). The stdlib's `ThreadFn` (`lib/threads.hc`) uses the keyword-less form.
`#exe { … }` runs
HolyC at compile time via the interpreter and splices its stdout.
**Still absent:** most of the TempleOS core/standard library and DolDoc.

Worked programs live in `examples/*.hc` (documentation / the human-readable tour). They are
also **ported into `tests/cases/ported_examples/`** so the integration suite compiles each
one natively and asserts byte-for-byte equality with the interpreter — the catch-all, now
one `#[test]` per file alongside the ~900 other `tests/cases/**/*.hc` programs, that keeps
backend changes honest. The `examples/` directory itself is no longer referenced by any
test; the test inputs are the `.hc` files under `tests/cases/`.
