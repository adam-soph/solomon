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
//! library with a defined algorithm. The algebraic float ops that *are* exactly
//! reproducible (`Sqrt`/`Floor`/`Ceil`/`Round`/`Fabs`) stay.

use crate::ast::Type;

/// The signature of a builtin function.
pub struct BuiltinSig {
    pub name: &'static str,
    pub ret: Type,
    /// Minimum number of arguments required.
    pub min_args: usize,
    pub varargs: bool,
}

/// Every builtin and its signature.
pub fn all() -> Vec<BuiltinSig> {
    let mut sigs = vec![
        // `Print(fmt, ...)` — printf-style output.
        BuiltinSig {
            name: "Print",
            ret: Type::U0,
            min_args: 1,
            varargs: true,
        },
        // The printf-family string builders. Variadic — they consume `...`, which
        // HolyC has no `va_arg` for, so they cannot (yet) be ordinary library code.
        // `StrPrint(dst, fmt, ...) -> dst` (sprintf into dst).
        BuiltinSig {
            name: "StrPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: true,
        },
        // `CatPrint(dst, fmt, ...) -> dst` (sprintf-append at dst + StrLen(dst)).
        BuiltinSig {
            name: "CatPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: true,
        },
        // `MStrPrint(fmt, ...) -> U8*` (asprintf into a fresh right-sized buffer).
        BuiltinSig {
            name: "MStrPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: true,
        },
        // `StrToF64(U8*) -> F64` / `F64ToStr(F64, U8*) -> U8*` — float parse/format;
        // need the correctly-rounded bignum machinery (shared with `Print`'s %g), so
        // they stay builtins for now.
        BuiltinSig {
            name: "StrToF64",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "F64ToStr",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // Clock/time primitives — impure (they read the OS clock or sleep), so
        // unlike every other builtin they are *not* reproducible across backends:
        // the byte-for-byte conformance is relaxed for these (tested by property,
        // not value). `UnixNS()` is wall-clock ns since the Unix epoch
        // (CLOCK_REALTIME), `NanoNS()` monotonic ns (CLOCK_MONOTONIC, for
        // durations), `Sleep(ns)` suspends the thread.
        BuiltinSig {
            name: "UnixNS",
            ret: Type::I64,
            min_args: 0,
            varargs: false,
        },
        BuiltinSig {
            name: "NanoNS",
            ret: Type::I64,
            min_args: 0,
            varargs: false,
        },
        BuiltinSig {
            name: "Sleep",
            ret: Type::U0,
            min_args: 1,
            varargs: false,
        },
        // Memory ops (libc memcpy/memmove/memset/memcmp/memchr/memmem).
        BuiltinSig {
            name: "MemCpy",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        BuiltinSig {
            name: "MemMove",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        BuiltinSig {
            name: "MemSet",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        BuiltinSig {
            name: "MemCmp",
            ret: Type::I64,
            min_args: 3,
            varargs: false,
        },
        BuiltinSig {
            name: "MemFind",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        BuiltinSig {
            name: "MemSearch",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 4,
            varargs: false,
        },
        // Heap (mmap bump allocator on the freestanding targets, libc malloc/free else).
        BuiltinSig {
            name: "MAlloc",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "Free",
            ret: Type::U0,
            min_args: 1,
            varargs: false,
        },
        // `ToUpper(I64) -> I64` / `ToLower(I64) -> I64` — ASCII case conversion.
        BuiltinSig {
            name: "ToUpper",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "ToLower",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        // The exactly-reproducible algebraic float ops — backed by single hardware
        // instructions (FSQRT/FRINT*, sqrtsd/roundsd), so unlike pure library code
        // they stay builtins to keep the correctly-rounded IEEE result.
        BuiltinSig {
            name: "Sqrt",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "Fabs",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "Floor",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "Ceil",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "Round",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        // `RandU64() -> U64` — deterministic splitmix64 (fixed seed), mirrored by the
        // native backends so the sequence is identical everywhere.
        BuiltinSig {
            name: "RandU64",
            ret: Type::U64,
            min_args: 0,
            varargs: false,
        },
        // `ArgC() -> I64` / `ArgV(I64) -> U8*` — the captured command line.
        BuiltinSig {
            name: "ArgC",
            ret: Type::I64,
            min_args: 0,
            varargs: false,
        },
        BuiltinSig {
            name: "ArgV",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
    ];
    // The `Is*` ctype classification predicates — each `(I64) -> I64` returning
    // 0 or 1. Computed inline in both backends (see `ctype_ranges`).
    sigs.extend(CTYPE_NAMES.iter().map(|&name| BuiltinSig {
        name,
        ret: Type::I64,
        min_args: 1,
        varargs: false,
    }));
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
        "Floor" => "_floor",
        "Ceil" => "_ceil",
        "Round" => "_round",
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
