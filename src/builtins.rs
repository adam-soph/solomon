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
    vec![
        // `Print(fmt, ...)` — printf-style output.
        BuiltinSig {
            name: "Print",
            ret: Type::U0,
            min_args: 1,
            varargs: true,
        },
        // `StrPrint(U8 *dst, U8 *fmt, ...) -> U8*` — printf-style formatting into
        // `dst` (libc `sprintf`); returns `dst`. Specially lowered like `Print`.
        BuiltinSig {
            name: "StrPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: true,
        },
        // `CatPrint(U8 *dst, U8 *fmt, ...) -> U8*` — like `StrPrint` but *appends*
        // to `dst` (formats into `dst + StrLen(dst)`); returns `dst`.
        BuiltinSig {
            name: "CatPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: true,
        },
        // `MStrPrint(U8 *fmt, ...) -> U8*` — format into a freshly `MAlloc`d,
        // right-sized buffer (asprintf-style); returns the new buffer. Specially
        // lowered (measure with `snprintf`, `malloc`, then `sprintf`).
        BuiltinSig {
            name: "MStrPrint",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: true,
        },
        // `StrToI64(U8 *str) -> I64` — parse a base-10 integer (libc `atoll`).
        BuiltinSig {
            name: "StrToI64",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        // `StrToF64(U8 *str) -> F64` — parse a floating value (libc `atof`).
        BuiltinSig {
            name: "StrToF64",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        // `MemMove(U8* dst, U8* src, I64 n) -> U8*` — copy `n` bytes, overlap-safe
        // (libc `memmove`).
        BuiltinSig {
            name: "MemMove",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        // `StrToUpper(U8*)` / `StrToLower(U8*) -> U8*` — ASCII-case a string in
        // place; return it. No libc equivalent — emitted as an inline loop.
        BuiltinSig {
            name: "StrToUpper",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
        BuiltinSig {
            name: "StrToLower",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
        // `StrRev(U8*) -> U8*` — reverse a string in place; return it. No libc
        // equivalent — emitted as an inline two-pointer loop.
        BuiltinSig {
            name: "StrRev",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
        // `MemFind(U8* buf, I64 c, I64 n) -> U8*` — pointer to the first byte
        // equal to `c` in `buf[0..n]`, or `NULL` (libc `memchr`).
        BuiltinSig {
            name: "MemFind",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        // `MemSearch(U8* hay, I64 hlen, U8* needle, I64 nlen) -> U8*` — pointer to
        // the first occurrence of the `needle` byte sequence in `hay`, or `NULL`
        // (libc `memmem`).
        BuiltinSig {
            name: "MemSearch",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 4,
            varargs: false,
        },
        // `I64ToStr(I64 n, U8* buf) -> U8*` / `F64ToStr(F64 f, U8* buf) -> U8*` —
        // format a number into `buf` (decimal / `%g`); return `buf`. Specially
        // lowered to `sprintf` with a fixed format.
        BuiltinSig {
            name: "I64ToStr",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        BuiltinSig {
            name: "F64ToStr",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // `StrChr(U8* str, I64 c) -> U8*` / `StrLastChr(...) -> U8*` — pointer to
        // the first / last `c` in the NUL-terminated `str`, or `NULL` (libc
        // `strchr` / `strrchr`; the terminating NUL counts, so `c == 0` finds it).
        BuiltinSig {
            name: "StrChr",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        BuiltinSig {
            name: "StrLastChr",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // `StrSpn(U8* str, U8* set) -> I64` / `StrCSpn(...) -> I64` — length of the
        // initial run of `str` made of chars that are in / not in `set` (libc
        // `strspn` / `strcspn`).
        BuiltinSig {
            name: "StrSpn",
            ret: Type::I64,
            min_args: 2,
            varargs: false,
        },
        BuiltinSig {
            name: "StrCSpn",
            ret: Type::I64,
            min_args: 2,
            varargs: false,
        },
        // `Abs(I64) -> I64` — integer absolute value (libc `llabs`).
        BuiltinSig {
            name: "Abs",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        // `Sqrt(F64) -> F64` — square root (libc `sqrt`).
        BuiltinSig {
            name: "Sqrt",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        // `StrLen(U8*) -> I64` — length of a NUL-terminated string (libc `strlen`).
        BuiltinSig {
            name: "StrLen",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        // `StrCmp(U8*, U8*) -> I64` — string comparison, normalized to the sign
        // of the difference (-1, 0, 1) so both backends agree (libc `strcmp`).
        BuiltinSig {
            name: "StrCmp",
            ret: Type::I64,
            min_args: 2,
            varargs: false,
        },
        // `StrCpy(U8* dst, U8* src) -> U8*` — copy a string, returns `dst` (libc
        // `strcpy`).
        BuiltinSig {
            name: "StrCpy",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // `MAlloc(I64) -> U8*` — allocate a byte buffer (libc `malloc`).
        BuiltinSig {
            name: "MAlloc",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 1,
            varargs: false,
        },
        // `Free(U8*) -> U0` — release a `MAlloc`d buffer (libc `free`).
        BuiltinSig {
            name: "Free",
            ret: Type::U0,
            min_args: 1,
            varargs: false,
        },
        // `StrCat(U8* dst, U8* src) -> U8*` — append `src` to `dst` (libc `strcat`).
        BuiltinSig {
            name: "StrCat",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // `MemCpy(U8* dst, U8* src, I64 n) -> U8*` — copy `n` bytes (libc `memcpy`).
        BuiltinSig {
            name: "MemCpy",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        // `MemSet(U8* dst, I64 c, I64 n) -> U8*` — set `n` bytes to `c` (libc `memset`).
        BuiltinSig {
            name: "MemSet",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
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
        // `MemCmp(U8*, U8*, I64 n) -> I64` — compare `n` bytes, normalized to a
        // sign in `{-1, 0, 1}` (libc `memcmp`).
        BuiltinSig {
            name: "MemCmp",
            ret: Type::I64,
            min_args: 3,
            varargs: false,
        },
        // Rounding (`F64 -> F64`). Exactly reproducible (hardware/`roundsd`), so
        // unlike the transcendentals these stay as builtins.
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
        // `StrFind(U8* haystack, U8* needle) -> U8*` — a pointer to the first
        // occurrence of `needle` in `haystack`, or `NULL`. Argument order matches
        // libc `strstr` 1:1.
        BuiltinSig {
            name: "StrFind",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 2,
            varargs: false,
        },
        // `StrNCmp(U8*, U8*, I64 n) -> I64` — compare up to `n` chars, normalized
        // to a sign in `{-1, 0, 1}` (libc `strncmp`).
        BuiltinSig {
            name: "StrNCmp",
            ret: Type::I64,
            min_args: 3,
            varargs: false,
        },
        // `StrNCpy(U8* dst, U8* src, I64 n) -> U8*` — copy up to `n` chars,
        // NUL-padding (libc `strncpy`).
        BuiltinSig {
            name: "StrNCpy",
            ret: Type::Ptr(Box::new(Type::U8)),
            min_args: 3,
            varargs: false,
        },
        // `Fabs(F64) -> F64` — floating absolute value (libc `fabs`).
        BuiltinSig {
            name: "Fabs",
            ret: Type::F64,
            min_args: 1,
            varargs: false,
        },
        // `Sign(I64) -> I64` — `-1`, `0`, or `1`. A computed builtin (no libc
        // equivalent): the arm64 backend emits it inline.
        BuiltinSig {
            name: "Sign",
            ret: Type::I64,
            min_args: 1,
            varargs: false,
        },
        // `RandU64() -> U64` — a deterministic splitmix64 PRNG (fixed seed), so
        // its sequence is identical in both backends. A computed builtin (the
        // arm64 backend emits it inline against a hidden global state word).
        BuiltinSig {
            name: "RandU64",
            ret: Type::U64,
            min_args: 0,
            varargs: false,
        },
    ]
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
        "Abs" => "_llabs",
        "Sqrt" => "_sqrt",
        "StrLen" => "_strlen",
        "StrCmp" => "_strcmp",
        "StrCpy" => "_strcpy",
        "MAlloc" => "_malloc",
        "Free" => "_free",
        "StrCat" => "_strcat",
        "MemCpy" => "_memcpy",
        "MemSet" => "_memset",
        "ToUpper" => "_toupper",
        "ToLower" => "_tolower",
        "MemCmp" => "_memcmp",
        "Floor" => "_floor",
        "Ceil" => "_ceil",
        "Round" => "_round",
        "StrFind" => "_strstr",
        "StrNCmp" => "_strncmp",
        "StrNCpy" => "_strncpy",
        "Fabs" => "_fabs",
        "StrToI64" => "_atoll",
        "StrToF64" => "_atof",
        "MemMove" => "_memmove",
        "MemFind" => "_memchr",
        "MemSearch" => "_memmem",
        "StrChr" => "_strchr",
        "StrLastChr" => "_strrchr",
        "StrSpn" => "_strspn",
        "StrCSpn" => "_strcspn",
        // `Sign`/`RandU64` (computed inline), `StrToUpper`/`StrToLower`/`StrRev`
        // (inline loops), and `Print`/`StrPrint`/`CatPrint`/`MStrPrint`/
        // `I64ToStr`/`F64ToStr` (specially lowered) are not here.
        _ => return None,
    })
}

/// Whether `name` is a builtin function.
pub fn is_builtin(name: &str) -> bool {
    all().iter().any(|b| b.name == name)
}
