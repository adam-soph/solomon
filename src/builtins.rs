//! Intrinsic functions solomon provides without a user definition.
//!
//! This is the single source of truth shared by semantic analysis (which
//! registers their signatures so calls type-check), the interpreter (which
//! implements their behaviour), and the AArch64 backend (which lowers them — most
//! via `libc_symbol`). Adding an intrinsic means an entry here plus a behaviour
//! arm in `interp` and, for a libc-backed one, a `libc_symbol` mapping
//! (or special-cased emission). These will be superseded by the real core /
//! standard library when it lands.
//!
//! Every intrinsic here has **backend-independent, solomon-defined semantics**
//! (e.g. `Sqrt` correctly-rounded, formatting pinned by [`crate::fmt`]). That is
//! deliberately why the **transcendental** math
//! functions (`Sin`/`Cos`/`Pow`/`Exp`/`Ln`/…) are *not* here: their only meaning
//! would be "whatever the host libm computes," which isn't reproducible across
//! platforms (IEEE 754 doesn't require correctly-rounded transcendentals) and
//! can't exist in a freestanding target. They belong in a future HolyC standard
//! library with a defined algorithm. Only the *irreducible* algebraic float ops
//! stay: `Sqrt` (a correctly-rounded hardware instruction) and `Fabs` (a sign-bit
//! clear the interpreter models specially). `Floor`/`Ceil`/`Round` are reducible
//! (an exact I64 cast + adjust), so they moved to `lib/math.hc`.

use crate::ast::Type;

/// The signature of a builtin function.
pub struct BuiltinSig {
    pub name: &'static str,
    pub ret: Type,
    /// Declared parameter types — the required prefix; the count is the minimum
    /// arity, and `varargs` adds a trailing `...`. (Argument *types* aren't strictly
    /// enforced — HolyC is weakly typed — but they give each builtin a real
    /// signature for `&Func`, error messages, and future checks.)
    pub params: Vec<Type>,
    pub varargs: bool,
}

/// Every builtin and its signature.
pub fn all() -> Vec<BuiltinSig> {
    let u8p = || Type::Ptr(Box::new(Type::U8));
    let i64 = || Type::I64;
    let f64 = || Type::F64;
    // (name, ret, params, varargs)
    let sig = |name, ret, params: Vec<Type>, varargs| BuiltinSig {
        name,
        ret,
        params,
        varargs,
    };
    let sigs = vec![
        // `Print(fmt, ...)` — printf-style output.
        sig("Print", Type::U0, vec![u8p()], true),
        sig("StrPrint", u8p(), vec![u8p(), u8p()], true), // (dst, fmt, ...) -> dst
        sig("CatPrint", u8p(), vec![u8p(), u8p()], true), // sprintf-append
        sig("MStrPrint", u8p(), vec![u8p()], true),       // asprintf into a fresh buffer
        // Float conversion is *not* here: both directions are pure HolyC in
        // `lib/strconv.hc`/`lib/cstr.hc`. `StrToF64` is a correctly-rounded bignum
        // `atof` (no host libc), and its inverse `F64ToStr` is a `StrPrint("%g")`
        // wrapper — so neither needs to be a primitive.
        // Clock/time primitives — impure (read the OS clock or sleep), so
        // non-reproducible: conformance is by property, not value.
        sig("UnixNS", i64(), vec![], false), // wall-clock ns (CLOCK_REALTIME)
        sig("NanoNS", i64(), vec![], false), // monotonic ns (CLOCK_MONOTONIC)
        sig("Sleep", Type::U0, vec![i64()], false),
        // Heap (mmap bump allocator freestanding, libc malloc/free hosted). The
        // only irreducible memory primitives — the byte-loop ops (`MemCpy`/`MemMove`/
        // `MemSet`/`MemCmp`/`MemFind`/`MemSearch`) are pure HolyC in `lib/mem.hc`.
        sig("MAlloc", u8p(), vec![i64()], false),
        sig("Free", Type::U0, vec![u8p()], false),
        // `HeapExtend(ptr, old, new)` grows the block at `ptr` (originally `old`
        // bytes) to `new` bytes *in place*, returning `ptr` — but only when that's
        // free (a bump allocator extending its last block); otherwise NULL. The one
        // irreducible bit of a `realloc`: the move-and-copy fallback is pure HolyC
        // (`ReAlloc` in `lib/mem.hc`). NULL on the libc/interp heaps (no in-place
        // API), so they always take the copy path — where `Free` actually reclaims.
        sig("HeapExtend", u8p(), vec![u8p(), i64(), i64()], false),
        // `MSize(ptr)` returns the byte size that was requested for the block at
        // `ptr` (a TempleOS heap primitive). It needs the allocator to *track* sizes,
        // so when a program uses it `MAlloc` prepends an 8-byte size header (gated on
        // `MSize` usage, so size-agnostic programs keep the lean header-free heap);
        // the interpreter tracks sizes in a side table.
        sig("MSize", i64(), vec![u8p()], false),
        // The two irreducible algebraic float primitives: `Sqrt` (a correctly-
        // rounded hardware instruction) and `Fabs` (a sign-bit clear the interpreter
        // models specially — it can't byte-pun a local). The reducible rounding ops
        // (`Floor`/`Ceil`/`Round`, exact via an I64 cast) live in `lib/math.hc`, as
        // does the splitmix64 `RandU64`; `ToUpper`/`ToLower` and the `Is*` ctype
        // predicates (ASCII range checks) live in `lib/ctype.hc`.
        sig("Sqrt", f64(), vec![f64()], false),
        sig("Fabs", f64(), vec![f64()], false),
        // The captured command line.
        sig("ArgC", i64(), vec![], false),
        sig("ArgV", u8p(), vec![i64()], false),
        // Variadic-argument access, valid only inside a `...` function. Varargs are
        // raw 64-bit slots; the accessor picks the type (as in C's `va_arg` — read
        // back the type you passed). `VarArgCnt()` is the number passed.
        sig("VarArgCnt", i64(), vec![], false),
        sig("VarArgI64", i64(), vec![i64()], false),
        sig("VarArgF64", f64(), vec![i64()], false),
        sig("VarArg", u8p(), vec![i64()], false),
    ];
    sigs
}

/// The C library symbol a builtin lowers to in the native backend, if it is
/// implemented by calling into libc (rather than inline or via `printf`).
pub fn libc_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "Sqrt" => "_sqrt",
        "MAlloc" => "_malloc",
        "Free" => "_free",
        "Fabs" => "_fabs",
        // `ArgC`/`ArgV` (read hidden globals) and `Print`/`StrPrint`/`CatPrint`/
        // `MStrPrint` (specially lowered) are not here. The string, memory, ctype and
        // PRNG ops — plus float conversion `F64ToStr` (a `StrPrint("%g")` wrapper) and
        // `StrToF64` (a correctly-rounded bignum `atof`) — are no longer builtins at
        // all: they live in `lib/cstr.hc`/`mem.hc`/`ctype.hc`/`strconv.hc` / `math.hc`
        // as pure HolyC.
        _ => return None,
    })
}

/// Whether `name` is a builtin function.
/// The builtin names, built once and cached. `all()` allocates a fresh registry
/// `Vec` (it's used for one-shot signature seeding in sema), so `is_builtin` —
/// which the interpreter calls on **every** builtin call — goes through this cache
/// instead, avoiding a per-call allocation of the whole registry. (`BuiltinSig`'s
/// `Type` isn't `Sync`, so only the `&'static str` names are cached.)
fn names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| all().iter().map(|b| b.name).collect())
}

pub fn is_builtin(name: &str) -> bool {
    names().contains(&name)
}

/// The declared return type of a builtin (`None` if unknown). Compile-time only
/// (per builtin call site lowered), so it rebuilds the registry rather than caching
/// the non-`Sync` `Type`.
pub fn ret_of(name: &str) -> Option<Type> {
    all().into_iter().find(|b| b.name == name).map(|b| b.ret)
}
