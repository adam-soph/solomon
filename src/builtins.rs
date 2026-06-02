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
//! (e.g. `StrCmp` normalized to a sign, `RandU64` a fixed splitmix64, formatting
//! pinned by [`crate::fmt`]). That is deliberately why the **transcendental** math
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
    let mut sigs = vec![
        // `Print(fmt, ...)` — printf-style output.
        sig("Print", Type::U0, vec![u8p()], true),
        // The printf-family string builders. Variadic — they consume `...`, which
        // HolyC has no `va_arg` for, so they can't yet be ordinary library code.
        sig("StrPrint", u8p(), vec![u8p(), u8p()], true), // (dst, fmt, ...) -> dst
        sig("CatPrint", u8p(), vec![u8p(), u8p()], true), // sprintf-append
        sig("MStrPrint", u8p(), vec![u8p()], true),       // asprintf into a fresh buffer
        // Float parse/format (correctly-rounded bignum, shared with `Print`'s %g).
        sig("StrToF64", f64(), vec![u8p()], false),
        sig("F64ToStr", u8p(), vec![f64(), u8p()], false),
        // Clock/time primitives — impure (read the OS clock or sleep), so
        // non-reproducible: conformance is by property, not value.
        sig("UnixNS", i64(), vec![], false), // wall-clock ns (CLOCK_REALTIME)
        sig("NanoNS", i64(), vec![], false), // monotonic ns (CLOCK_MONOTONIC)
        sig("Sleep", Type::U0, vec![i64()], false),
        // Memory ops (libc memcpy/memmove/memset/memcmp/memchr/memmem).
        sig("MemCpy", u8p(), vec![u8p(), u8p(), i64()], false),
        sig("MemMove", u8p(), vec![u8p(), u8p(), i64()], false),
        sig("MemSet", u8p(), vec![u8p(), i64(), i64()], false),
        sig("MemCmp", i64(), vec![u8p(), u8p(), i64()], false),
        sig("MemFind", u8p(), vec![u8p(), i64(), i64()], false),
        sig("MemSearch", u8p(), vec![u8p(), i64(), u8p(), i64()], false),
        // Heap (mmap bump allocator freestanding, libc malloc/free hosted).
        sig("MAlloc", u8p(), vec![i64()], false),
        sig("Free", Type::U0, vec![u8p()], false),
        // ASCII case conversion.
        sig("ToUpper", i64(), vec![i64()], false),
        sig("ToLower", i64(), vec![i64()], false),
        // The two irreducible algebraic float primitives: `Sqrt` (a correctly-
        // rounded hardware instruction) and `Fabs` (a sign-bit clear the interpreter
        // models specially — it can't byte-pun a local). The reducible rounding ops
        // (`Floor`/`Ceil`/`Round`, exact via an I64 cast) live in `lib/math.hc`.
        sig("Sqrt", f64(), vec![f64()], false),
        sig("Fabs", f64(), vec![f64()], false),
        // Deterministic splitmix64 PRNG (fixed seed), mirrored by the backends.
        sig("RandU64", Type::U64, vec![], false),
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
    // The `Is*` ctype predicates — each `(I64) -> I64` returning 0/1.
    sigs.extend(
        CTYPE_NAMES
            .iter()
            .map(|&name| sig(name, i64(), vec![i64()], false)),
    );
    sigs
}

/// The ASCII `Is*` character-classification builtins. Each class is a union of
/// inclusive byte ranges (see [`ctype_ranges`]) — the **single source of truth**
/// for classification: the interpreter tests membership and both native backends
/// emit the same range checks, so the result is identical everywhere. Defined for
/// the C/POSIX "C" locale (pure ASCII); a byte outside every range — including
/// high bytes and negative/EOF-like values — classifies as false (`0`).
pub const CTYPE_NAMES: &[&str] = &[
    "IsDigit", "IsAlpha", "IsAlNum", "IsSpace", "IsUpper", "IsLower", "IsXDigit", "IsPunct",
    "IsCntrl", "IsPrint", "IsGraph", "IsBlank",
];

/// The inclusive byte ranges defining each `Is*` predicate, or `None` for a name
/// that isn't a ctype predicate.
pub fn ctype_ranges(name: &str) -> Option<&'static [(u8, u8)]> {
    Some(match name {
        "IsDigit" => &[(b'0', b'9')],
        "IsUpper" => &[(b'A', b'Z')],
        "IsLower" => &[(b'a', b'z')],
        "IsAlpha" => &[(b'A', b'Z'), (b'a', b'z')],
        "IsAlNum" => &[(b'0', b'9'), (b'A', b'Z'), (b'a', b'z')],
        "IsXDigit" => &[(b'0', b'9'), (b'A', b'F'), (b'a', b'f')],
        "IsSpace" => &[(0x09, 0x0d), (b' ', b' ')], // \t \n \v \f \r and space
        "IsBlank" => &[(b'\t', b'\t'), (b' ', b' ')],
        "IsCntrl" => &[(0x00, 0x1f), (0x7f, 0x7f)],
        "IsPrint" => &[(b' ', 0x7e)], // space..'~'
        "IsGraph" => &[(0x21, 0x7e)], // '!'..'~'
        "IsPunct" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)], // graphic, not alnum
        _ => return None,
    })
}

/// Evaluate an `Is*` predicate on `c` — the interpreter path. `false` for a
/// non-predicate name or a byte outside `[0, 255]`.
pub fn ctype_test(name: &str, c: i64) -> bool {
    matches!(ctype_ranges(name), Some(ranges)
        if (0..=0xff).contains(&c)
            && ranges.iter().any(|&(lo, hi)| (lo as i64..=hi as i64).contains(&c)))
}

/// The hidden global holding the `RandU64` PRNG state in the native backend.
pub const RNG_STATE_GLOBAL: &str = "__solomon_holyc_rng_state";

/// splitmix64 step: advance `state` and return the next value.
pub fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// The C library symbol a builtin lowers to in the native backend, if it is
/// implemented by calling into libc (rather than inline or via `printf`).
pub fn libc_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "Sqrt" => "_sqrt",
        "MAlloc" => "_malloc",
        "Free" => "_free",
        "MemCpy" => "_memcpy",
        "MemSet" => "_memset",
        "ToUpper" => "_toupper",
        "ToLower" => "_tolower",
        "MemCmp" => "_memcmp",
        "Fabs" => "_fabs",
        "StrToF64" => "_atof",
        "MemMove" => "_memmove",
        "MemFind" => "_memchr",
        "MemSearch" => "_memmem",
        // `Sign`/`RandU64`/`ArgC`/`ArgV` and the `Is*` ctype predicates (computed
        // inline — libc's `isdigit` etc. return an unspecified nonzero, which would
        // diverge from the interpreter's 0/1), `StrToUpper`/`StrToLower`/`StrRev`
        // (inline loops), and `Print`/`StrPrint`/`CatPrint`/`MStrPrint`/`I64ToStr`/
        // `F64ToStr` (specially lowered) are not here.
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
