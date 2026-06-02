# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

solomon is a from-scratch reimplementation of **HolyC** (Terry Davis's TempleOS
language) in Rust. It is a real compiler front end plus a tree-walking
**interpreter** (`src/interp.rs`, the conformance oracle) and two hand-rolled
native code generators behind the **`Codegen`** trait (`src/codegen.rs`), each
named for its target — `arm64` (`aarch64-apple-darwin`, Mach-O via `cc`; and
`aarch64-unknown-linux`, a **freestanding** static ELF) and `x86_64`
(`x86_64-unknown-linux`, a freestanding static ELF; `x86_64-pc-windows`, a PE). A
codegen backend is an (arch, OS) pair, not just a CPU. **Both Linux targets are
freestanding — no libc, no linker, raw syscalls** (there are no gcc-linked
`-gnu`/`-musl` codegen variants; Darwin is the one hosted target, linking
libSystem via `cc`) — and match the interpreter byte-for-byte on all 18
examples.

## Commands

```sh
cargo build                 # debug build (CLIs: target/debug/{hcc,hci}; the lib crate is `solomon`)
cargo build --release       # release build
cargo test                  # whole suite
cargo test --test arm64_darwin     # one integration-test file (each tests/<name>.rs is a crate)
cargo test compiles_floats  # a single test by name (substring match)
cargo fmt                   # format (CI-relevant: keep the tree fmt-clean)
cargo clippy --all-targets  # lint — see note below
```

There are **two binaries** (`src/bin/{hcc,hci}.rs`, auto-discovered by Cargo):
**`hcc`** is the compiler — with **no subcommand it compiles a native binary for
the host** (`-o OUT`, default `a.out`); `--target TRIPLE` cross-compiles;
`check`/`ast`/`tokens` are front-end-only subcommands. **`hci`** is the interpreter
— `hci FILE [args]` runs the program (the conformance oracle). Run a specific
binary with `cargo run --bin`:

```sh
cargo run --bin hcc -- FILE.hc -o out          # compile: native binary for the host
cargo run --bin hcc -- --target x86_64-unknown-linux -o out FILE.hc  # cross-compile (a static ELF)
cargo run --bin hci -- FILE.hc                 # interpret (type-check then run)
cargo run --bin hcc -- check FILE.hc           # parse + sema only, report errors
cargo run --bin hcc -- ast FILE.hc             # dump the parsed AST
cargo run --bin hcc -- tokens FILE.hc          # raw lexer output (no preprocessing)
echo 'I64 Sq(I64 x){return x*x;} "%d\n", Sq(9);' | cargo run --bin hci   # reads stdin if no FILE
```

`make` wraps cargo for cross-compilation (`make`, `make all`, `make <triple>`,
`make dist`, `make macos-universal`); use `CARGO=cross` for non-host OSes. See
the Makefile / README for the target list.

### Important testing notes

- **`tests/arm64_darwin.rs` has two layers** (like `tests/x86_64_linux.rs`). The
  **structural checks** (`produces_a_valid_macho_arm64_object`,
  `main_is_framed_and_returns`) byte-inspect the emitted Mach-O object via
  `Arm64Darwin::object` (which stops *before* the `cc` link step), so they run on
  **every** host with no toolchain. The **end-to-end checks** shell out to `cc`
  and execute real Mach-O binaries (~14s), so they only run on an
  `aarch64-apple-darwin` host with `cc` and self-skip elsewhere (print "skipping"
  and pass). A green `cargo test` off an Apple-silicon Mac therefore exercises the
  AArch64 *emitter* but not *execution* — verify behavioral arm64 changes on an
  Apple-silicon Mac.
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
(`src/bin/hcc.rs`) wires these together per mode.

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
file's lexer can outlive the `&str` it was built from). `#include "file"` paths
resolve relative to the including file's directory; **angle** includes
`#include <name>` (the standard library) resolve against a *search path* instead
(the angle path is reassembled from its lexed tokens by `angle_path`, since unlike
the quoted form it isn't a single `Str` token). `parser::parse(src)` defaults the
base dir to the CWD with no search path; `parse_in_dir(src, dir)` sets the base
dir; `parse_with(src, dir, search)` sets both (the CLIs pass the input file's
parent and `solomon::stdlib_dirs()` — `SOLOMON_STDLIB`, then exe-relative `lib/`,
then `./lib`; `hcc -I DIR` prepends more). The library itself is HolyC in `lib/`.

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

### Interpreter & codegen backends

The tree-walking **interpreter** (`src/interp.rs`) is its own top-level module — it
is *not* a code generator. `Interpreter::new(sink).run(&program)` executes a
program (already type-checked) into an output sink; `run_to_string` is the safe
"check then run" entry. It is the conformance oracle.

`codegen.rs` defines `trait Codegen { name() -> triple; run(&Program) }` for the
**native code generators** (each constructed with an output path). Two implement
it — `Arm64Darwin` and `X64Linux` (the public structs; each builds an internal
`Cg` worker that holds the codegen state and emits). The x86-64 one covers
integers/pointers/classes/globals/F64/printf incl. `%f`/`%e`/`%g`/switch/goto + a
libc-free slice of the core-library builtins. **All 18 examples build natively on
both backends**; the only things they don't do are the deliberately-excluded
transcendentals. `CodegenError` (in `codegen.rs`) is the shared run/emit error.

- **`interp.rs`** — tree-walking `Interpreter<W: Write>`. Values are
  `Rc<RefCell<Value>>` cells; pointers use a region+offset model so arithmetic,
  indexing, and comparison work. `MAlloc` of a scalar type and every `union`
  instance are instead **raw byte buffers** (`Region::Heap` / `Value::Union`)
  accessed through `Place::Bytes`, so type punning and overlapping union fields
  match the native byte layout. `run_to_string` is the safe "check then run"
  entry. **This interpreter is the conformance oracle for the native backends** —
  when adding native-backend features, match its observable output.
- **`backend/arm64_darwin.rs`** — hand-emits AArch64 machine code (no LLVM/Cranelift/C),
  writes a Mach-O relocatable object by hand, and links with the system `cc`.
  Codegen is type-directed (uses `Expr::ty()` + the layout pass). It targets
  `aarch64-apple-darwin` specifically. Several **emission-time optimizations** (no
  separate pass — that would shift instruction positions and break label/fixup
  offsets): constant subexpressions fold to a single `load_imm` (`gen_expr` +
  `const_eval_i64`); **immediate-form arithmetic** (`try_imm_binop`) emits
  `add/sub/cmp #imm` and shift-by-immediate for `<expr> op <small constant>`
  instead of materializing the constant in a register, and **strength-reduces**
  `* / %` by a constant power of two (also in `try_imm_binop`): `* 2^k` → `lsl`,
  and for *unsigned* operands `/ 2^k` → `lsr` and `% 2^k` → `and #2^k-1` (signed
  `/`/`%` need a round-toward-zero bias, so they keep the generic SDIV/MSUB); a
  binary op whose rhs is a
  *simple operand* (`is_simple_operand`: literals, constant folds, scalar
  variables — codegen that provably touches only RES) keeps its lhs in T2 with a
  `mov` instead of a stack push/pop (the F64 analogue `is_simple_foperand` keeps
  the lhs in FT2 with an `fmov`, replacing the `push_f`/`pop_f` GPR+stack
  round-trip); and no-op `mov xd, xd` / zero-sized `sp` adjusts are elided. There
  is also a true **post-emission peephole pass**
  (`Asm::peephole`, run at the start of `finish` before fixups resolve): every
  `Asm` emitter tags its word with register def/use + a branch class
  (`inst_def`/`inst_use`/`inst_branch`, parallel to `words`; the conservative
  `emit` default is "reads everything, is a barrier", and the tagged `e_*`
  helpers refine it), and the pass runs a liveness scan over the scratch
  temporaries x9/x10 to drop dead `mov`s — either *removal* (the copy's target is
  never read again) or *fusion* (the instruction that produced the source is
  rewritten to write the destination directly). Removing a word remaps every
  stored word-index position (`label_pos` + the `.0` of
  fixups/adr_fixups/relocs; label ids and `TableRel`'s base are label indices, so
  they ride along through `label_pos`). All are behavior-preserving — the
  interpreter and the all-examples native conformance test are the oracle.
  Finally, a per-function **register promotion** pass (`plan_registers`, a light
  register allocator) keeps frequently-used scalar locals/params in callee-saved
  registers instead of frame slots, eliminating per-access load/store traffic (a
  big win in loops). Two independent pools: integer/pointer locals → **x19–x28**,
  F64 locals → the callee-saved double registers **d8–d15**. Candidates are
  non-address-taken scalars declared exactly once and referenced ≥2 times, where
  references are **loop-depth-weighted** (`8^depth`) — so a loop-invariant read
  (a loop bound, say) counts as hot and gets promoted even when its *static* count
  is one, dropping its per-iteration load (`RegAnalysis` scans the body for `&x`,
  weighted reference counts, declaration types, and per-name live intervals; the
  scan's `match`es are exhaustive so a new AST node can't silently hide an
  address-take). Allocation is **linear scan over live
  intervals** (`Cand`): locals whose ranges don't overlap **share** a register, so
  many sequential temporaries collapse onto one register (fewer distinct
  registers ⇒ less save/restore) and a function may promote more than a pool's
  worth of locals. When a pool runs out, the scan **spills** the coldest active
  interval (lowest loop-weighted refs) to a slot if the current candidate is
  hotter, handing it the register — a whole-range swap, not a split. Soundness
  rests on conservative *over*-approximated intervals —
  `[first reference … last reference]` (a declaration counts as the defining
  reference), widened to cover any structured loop they touch (loop-carried values
  stay live across the back-edge), and widened to the whole function when the body
  uses `goto`/labels (then nothing shares); two locals coalesce only when strictly
  disjoint. A promoted
  local has `VarLoc.reg = Some(r)` and no slot — the register's range identifies
  its file (`is_fp_reg`: r < 16 ⇒ a d-register), and the variable's type confirms
  it. Integer reads are `mov RES, r`, writes/inits/`++` `mov r, RES` (narrowed to
  the declared width via `gen_cast`, since there's no store to truncate); F64
  reads/writes are `fmov FRES, r` / `fmov r, FRES` and arithmetic targets the
  d-register directly. Params move from the arg register (`mov`/`fmov`) into `r`.
  The prologue spills each used callee-saved register near x29 with a single
  `stur`/`fstur`; every epilogue restores it with `ldur`/`fldur` (`cs_saves`).
  Promoted locals survive calls (incl. recursion) precisely because x19–x28 /
  d8–d15 are callee-saved.

- **`arm64/linux.rs` — freestanding `aarch64-unknown-linux`** (the only aarch64
  Linux codegen target; there is no gcc-linked libc variant). Shares the entire
  AArch64 emitter and `compile`/`build` drivers with Darwin via the `ArmTarget` seam
  (`freestanding()` + `write_executable()`; Darwin keeps the `write_object()` +
  `link()` path, which now have `unreachable!` trait defaults the freestanding
  target inherits); when set, `Cg.freestanding` flips the
  builtin/printf lowering from libc calls to **emitted AArch64 runtime**, and the
  driver writes a self-contained static ELF (own `_start`, raw syscalls `write`/
  `mmap`/`exit_group`, no linker) — the AArch64 analogue of the `x86_64` freestanding
  backend. Globals self-address via a single `ADR` to a fixed BSS slot (`adr_global_fs`
  + `global_adr_fixups`, no relocations; BSS reserved by `p_memsz > p_filesz`). The
  runtime (lazily emitted once at the end of `__text`, registered via `fs_routine`):
  the redirectable sink `OutWrite` (stdout `write`, or append to a `StrPrint` buffer
  via the `out_ptr` global), the full `FmtInt`/`FmtStr` (all flags/width/precision,
  mirroring `fmt::render_int`/`render_str`), the correctly-rounded **bignum float
  formatters** `FmtFloat` (`%f`) and `FmtFloatEg` (`%e`/`%g`) over a 48-limb `BIGNUM`
  with `BnMul`/`BnDiv10`/`BnShl`/`BnShr` (round-half-even in `BnShr`; `÷10` via a
  64-iteration shift/subtract since AArch64 has no 128÷64 divide), the bump allocator
  `MAlloc` over `mmap`, the string/memory ops, `StrFind`, the sprintf family, and the
  FP algebraic ops (`Sqrt`/`Fabs`/`Floor`/`Ceil`/`Round` = single AArch64 FP
  instructions). `tests/arm64_linux.rs` compiles **all 18 examples** freestanding and
  runs them under `docker --platform linux/arm64` (native on Apple silicon, no qemu),
  asserting byte-for-byte equality with the interpreter; it self-skips with no docker.

- **`backend/x86_64_linux.rs`** — a second native target (Linux/x86-64, *integers
  reached by `--target x86_64-unknown-linux` (CLI) or `X64Linux::new(path)`. It hand-emits
  x86-64 machine code and writes a **self-contained static ELF executable** — its
  own `_start` runs the program and calls the `exit` syscall, so there is no
  linker, no libc, and no relocations (`[ELF header | one PT_LOAD | code |
  strings | BSS]`, mapped R+W+X at `0x400000`; jumps/calls use rel32 fixups, and
  string/global references RIP-relative disp32s, all resolved in `Asm::finish`).
  Implemented: top-level code and **functions** (recursion, ≤6 integer/pointer
  args via the System V registers `rdi…r9`), **locals** (a `push rbp` frame,
  `[rbp-off]`), **control flow** (`if`/`else`/`while`/`for`/`do`/`break`/`continue`,
  `switch` — single/`lo ... hi`-range `case`s, `default`, and the `start:`/`end:`
  sub-labels, lowered to a compare-chain like arm64; `goto`/labels, pre-collected
  per function so forward jumps resolve),
  **comparisons** + short-circuit `&&`/`||`/`!`, unary `- + ~ ++ --`, the
  binary `+ - * / % & | ^ << >>` (`>>`/`/`/`%` and the relational ops are
  **signedness-directed** — `sar`/`idiv` vs `shr`/`div`, signed vs unsigned
  condition codes, keyed off the operand types as in the other backends), and
  **pointers & arrays** — `&x`, `*p`, `a[i]`
  (incl. 2-D), pointer arithmetic (pointee-scaled) and difference, width-aware
  narrow loads/stores (`movsx`/`movzx`), and array parameters that decay to a
  by-reference pointer (the `indirect` `VarLoc`), **classes & unions** (`repr(C)`
  offsets/sizes from the shared layout pass: `a.x` and `p->x` member access via
  `gen_addr`, nested fields, `sizeof`, whole-class assignment, **by-value**
  parameters lowered to a `rep movsb` copy — `class` rvalues are carried by
  address, the callee copies its arg into a local slot (in two passes so a class
  copy can't clobber later args still in arg registers) — and **by-value returns**
  (**sret**: the caller allocates a result temp and hands its address to the
  callee in `r11`; the callee saves that pointer and `rep movsb`s the result
  through it on `return`; member access on a class-returning call like `Mk().x`
  works since the rvalue is its address); arrays of classes and
  heap-free linked lists; union aliasing, with anonymous-embedded unions resolved
  through their promoted field offset), **globals** (top-level variables are
  allocated in a zero-filled BSS region that follows the strings in the address
  space — `p_memsz > p_filesz` reserves it, never in the file — addressed by
  RIP-relative disp32 like strings (`Asm::lea_global` + `global_fixups`), so
  functions read/write them and `Codegen::declare_global` lays them out
  `repr(C)`; only their initialisers run, in program order, in `_start`),
  **F64** (SSE2: `xmm0`/`xmm1` as the float result/temp, F64 args in `xmm0..xmm7`
  and returns in `xmm0` — counted independently of the GP arg registers;
  `gen_fexpr`/`gen_foperand` mirror the arm64 backend, with arithmetic, `-`,
  comparisons via `ucomisd`+unsigned `setcc`, and signedness-directed int↔float
  conversions (`cvtsi2sd`, `cvttsd2si`, and a 2^63-split `gen_f64_to_u64` for
  unsigned targets); float literals materialise as `mov`+`movq` with no constant
  pool), plus
  **printing** — a bare string prints verbatim, `"fmt", …`/`Print("fmt", …)` are
  printf-style with the **full** `%[flags][width][.prec]conv` grammar (flags
  `- 0 + space #`, `*` width/precision from args) for `%d %i %u %x %X %o %c %s
  %f %e %g %%` — the float forms are **correctly-rounded** via a 48-limb `BIGNUM`,
  matching Rust's `{:.P}`/`{:.Pe}` byte-for-byte (incl. round-half-to-even ties).
  `%f` (`emit_fmt_float` + `emit_bn_*`) builds J = round(m·2^e·10^P) by ×5^P then a
  binary shift with round-half-even, extracts decimal digits, and places the
  point. `%e`/`%g` (`emit_fmt_float_eg`) work from the value's **exact** finite
  decimal expansion (a double `m·2^e` is the dyadic rational `Dint·10^pe`, so
  `Dint = m·5^(−e)` is integer) — extract all digits, round the digit string to N
  significant figures (half-even, with carry-overflow bumping the exponent), then
  format `d.dddde±XX` (`%e`) or choose fixed/scientific and trim trailing zeros
  (`%g`). It's a stack machine in `rax` (left operand spilled
  to the machine stack while the right is computed, so values survive nested
  calls); an lvalue's address comes from `gen_addr`, with width-aware load/store
  through it. Type sizes/strides come from the [layout pass](crate::layout).
  Printing needs no libc: a tiny emitted runtime — `fmt_int`/`fmt_str` (emitted
  once, on demand), hand-assembled to **mirror `fmt::render_int`/`render_str`
  byte-for-byte** off the shared `fmt::parse` spec, building the padded field in a
  BSS scratch buffer and handing it to a single output sink (`out_write`): to the
  `write` syscall when the `out_ptr` global is 0, else appended to a destination
  buffer (advancing `out_ptr`) — so the **same** format machinery drives both
  `Print` and the sprintf family. The format routines
  are written with a small generic register-encoder layer added to `Asm`
  (`mov_rr`/`mov_ri`/`alu_rr`/byte loads-stores, etc.). String literals live
  *after* the code, RIP-relative addressed; `Asm::finish` resolves rel32 branch
  fixups, the string references and the global references, then appends the string
  bytes (the BSS follows in vaddr space). A **libc-free slice of the core-library
  builtins** is lowered inline or to emitted runtime routines (`gen_builtin` +
  `emit_rt_routines`): `MAlloc`/`Free` (a bump allocator over `mmap`'d 1 MiB
  chunks; `Free` is a no-op), the string/memory ops (`StrLen`/`StrCmp`/`StrCpy`/
  `StrCat`/`StrNCmp`/`StrNCpy`/`StrChr`/`StrLastChr`/`StrFind`/`StrSpn`/`StrCSpn`/
  `StrToUpper`/`StrToLower`/`StrRev`, `MemCpy`/`MemMove`/`MemSet`/`MemCmp`/
  `MemFind`/`MemSearch`) as byte-at-a-time loop routines, `ToUpper`/`ToLower`/
  `Abs`/`Sign`, the `Is*` ctype predicates (`emit_rt_ctype`, one inline range-check
  routine per class from the shared `builtins::ctype_ranges` table), `RandU64` (the
  shared splitmix64 over a zero-seeded BSS word, so
  its sequence matches the interpreter), SSE `Sqrt`/`Fabs`, and the **sprintf
  family** `StrPrint`/`CatPrint`/`I64ToStr` (printf into a buffer via the output
  sink; `CatPrint` appends at `dst + StrLen(dst)`). The transcendentals aren't
  builtins at all (see the `builtins.rs` note — they're excluded project-wide, not
  just here), so nothing math-related is missing from this backend; the algebraic
  ops `Sqrt`/`Fabs`/`Floor`/`Ceil`/`Round` are all done — `Floor`/`Ceil` are a
  single `roundsd` (modes 1/2), and `Round` is `trunc` (`roundsd` mode 3) plus a
  `copysign` bump so it rounds **half away from zero** like the interpreter's
  `f64::round` and arm64's `frinta` (`roundsd`'s nearest mode is half-to-even, which
  would diverge).
  `MStrPrint` (a **growing sink** like libc's `vasprintf`: `MAlloc` a small owned
  buffer, then format in one pass while the sink reallocs-and-copies on overflow —
  `Helper::GrowSink`, doubling capacity — so the result is just `out_base` when
  done; no measure pass, no fixed cap) and `F64ToStr` (`%g` via the shared sink)
  are implemented too, so the whole core-library builtin set is covered. (**All 18 examples compile and run
  natively**, matching the interpreter.) Tests in
  `tests/x86_64_linux.rs` verify the ELF structure
  on any host and **execute** the produced binary — directly on a `linux`/`x86_64`
  host, otherwise via one `docker run --platform linux/amd64` container (the
  static ELF needs no libc) — checking exit codes *and* stdout byte-for-byte
  against the interpreter (incl. whole example programs), self-skipping when
  neither runner is available.

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
`MemCpy`/`MemMove`/`MemSet`/`MemCmp`/`MemFind`/`MemSearch`/`MAlloc`/`Free`; char
`ToUpper`/`ToLower` and the ASCII `Is*` classification predicates (`IsDigit`/
`IsAlpha`/`IsAlNum`/`IsSpace`/`IsUpper`/`IsLower`/`IsXDigit`/`IsPunct`/`IsCntrl`/
`IsPrint`/`IsGraph`/`IsBlank`) — these are *computed inline* in both backends from a
shared range table (`builtins::ctype_ranges`), **not** libc-backed, since libc's
`isdigit` etc. return an unspecified nonzero that wouldn't match the interpreter's
0/1; and the exactly-reproducible float ops
`Sqrt`/`Floor`/`Ceil`/`Round`/`Fabs`/`Sign`; plus `RandU64`. (The **transcendentals** — `Sin`/`Cos`/`Pow`/`Exp`/`Ln`/… — are
deliberately *not* builtins: an intrinsic must have a portable, solomon-defined
value, but a transcendental's would be only "whatever the host libm computes,"
which isn't reproducible across platforms and can't exist in a freestanding
target. They instead live in the **HolyC standard library** (`lib/*.hc`, e.g.
`lib/math.hc`'s `Exp`/`Ln`/`Pow`/`Sin`/`Cos`) — ordinary HolyC built on the
deterministic F64 ops + these algebraic builtins, so each function's *defined
algorithm* computes identically on the interpreter and every backend; pull it in
with an angle include, `#include <math.hc>`. See the `builtins.rs` module doc.)
`NULL`/`TRUE`/`FALSE` are
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
it lowers through the generic path with no special-casing. The *computed*
builtins have no libc equivalent and are emitted inline by both backends:
`Sign` (`(x>0)-(x<0)`); the `Is*` ctype predicates (inline ASCII range checks from
the shared `builtins::ctype_ranges` table — deliberately not libc's `isdigit` etc.,
whose unspecified nonzero return wouldn't match the interpreter's 0/1); and
`RandU64` — a deterministic splitmix64 (`builtins::splitmix64`, fixed zero seed) so
its sequence is identical in both backends; the native backend keeps the state in a
hidden `RNG_STATE_GLOBAL` common symbol the interpreter mirrors with
`Interpreter::rng_state`. The **clock/time primitives** `UnixNS` (wall-clock ns,
CLOCK_REALTIME), `NanoNS` (monotonic ns, CLOCK_MONOTONIC) and `Sleep(ns)` are the
one **impure, non-reproducible** group: the clock differs between an interpreter
run and a native run, so the byte-for-byte conformance is *relaxed* for them —
they're tested by **property** (monotonic non-decreasing across a `Sleep`; wall
clock past 1970), never by interp-vs-native value comparison. The interpreter uses
`std::time`; the freestanding backends emit `clock_gettime`/`nanosleep` syscalls
(arm64 113/101, x86_64 228/35) over a 16-byte BSS timespec, folding `sec·1e9+nsec`;
Darwin calls libc `clock_gettime`/`nanosleep` over a stack timespec (macOS
`CLOCK_MONOTONIC`=6, not Linux's 1); Windows lowers them via kernel32
(`GetSystemTimePreciseAsFileTime` → ns since 1970, `GetTickCount64`×1e6, `Sleep`)
through the `OsTarget::emit_unix_ns`/`emit_mono_ns`/`emit_sleep` seam. The arm64 backend reaches libc
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
- **Chained range comparisons** are a pure parser desugar (`parse_binary`): a run
  of relational operators at the same precedence (`a < b < c`, `0 <= i < n`)
  becomes the conjunction `(a<b) && (b<c)` (`is_chain_cmp` selects `< <= > >=`;
  `==`/`!=` keep C's `(a==b)==c`). Interior operands are *cloned* into both
  comparisons (so `a < f() < b` calls twice). Sema and both backends are
  untouched — they only ever see the standard `&&`-of-comparisons AST.
- **Scalar stores coerce to the lvalue type** in the interpreter (`coerce_to` in
  `eval_init`/`eval_assign`): `I64 w = 3.14;` truncates to `3` and `F64 x = 5;`
  widens to `5.0`, matching the native backend (which truncates/widens in
  registers). Without this the interpreter kept the source `Value` and diverged
  on later arithmetic. `coerce_to` also decays a **string literal stored into a
  pointer** to one stable byte buffer, so pointer identity over it (`p - s`,
  `p == s`, `p++`) is consistent — mirroring the native backend's single `__text`
  copy (each `as_pointer` of a `Value::Str` would otherwise mint a fresh buffer).
- **Locals are zero-initialized** when declared without an initializer, in both
  backends: scalars and *aggregates alike* read back as 0 until written (interp
  fills its value; native zeroes the slot — `gen_zero_slot` for an aggregate, a
  store of 0 for a scalar). Reading an untouched local is therefore defined, not
  stack garbage. (Globals are linker-zeroed.)
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

Code generation is implemented for all three native targets (the default,
host-target build, or `--target TRIPLE` for a specific triple). The backends
compile the whole implemented
HolyC subset, including the `offset` keyword, brace aggregate initializers
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
interpreter output for the substantive ones), and `tests/arm64_darwin.rs`, whose
`native_matches_interp_for_every_example` compiles **every** example with the
native backend and asserts byte-for-byte equality with the interpreter — the
catch-all that keeps new examples and backend changes honest.
