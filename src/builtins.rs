//! The **builtin registry**: the few functions solomon provides without a user
//! definition and without an `#include` — sema seeds their signatures so calls
//! type-check, the interpreter implements them, and the backends lower them. It has
//! been pared all the way down to the primitives that can't be library functions at
//! all: the captured command line `ArgC`/`ArgV` (read hidden globals) and the
//! variadic-argument accessors `VarArg*` (need ABI support). Everything else is now
//! a **library function or intrinsic** ([`crate::intrinsics`]): the printf family
//! (`lib/fmt.hc`), the heap (`lib/mem.hc`), the clock (`lib/time.hc`), `Sqrt`/`Fabs`
//! and the rounding/transcendentals (`lib/math.hc`), float conversion
//! (`lib/strconv.hc`/`lib/cstr.hc`), and the string/memory/ctype ops.
//!
//! `libc_symbol` is kept here as a name→libc-symbol map for the hosted (Darwin)
//! arm64 lowering of the heap intrinsics (`MAlloc`→`_malloc`, `Free`→`_free`); it is
//! independent of the registry `all()`.

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
        // The printf family is *not* here: `Print`/`StrPrint`/`CatPrint`/`MStrPrint`
        // are **primitive intrinsics** declared in `lib/fmt.hc` (prototypes), lowered
        // by the backends via the shared `fmt` machinery — see `crate::intrinsics`.
        // Float conversion is *not* here: both directions are pure HolyC in
        // `lib/strconv.hc`/`lib/cstr.hc`. `StrToF64` is a correctly-rounded bignum
        // `atof` (no host libc), and its inverse `F64ToStr` is a `StrPrint("%g")`
        // wrapper — so neither needs to be a primitive.
        // The impure clock primitives are *not* here either: `UnixNS`/`NanoNS`/`Sleep`
        // are **primitive intrinsics** declared in `lib/time.hc`, lowered to syscalls
        // (freestanding) / libc / the Windows `OsTarget` seam — see `crate::intrinsics`.
        // The heap is *not* here: `MAlloc`/`Free`/`HeapExtend`/`MSize` are **primitive
        // intrinsics** declared in `lib/mem.hc`, lowered to an `mmap` bump allocator
        // (freestanding) or libc `malloc`/`free` (hosted) — see `crate::intrinsics`.
        // Nor are the algebraic floats: `Sqrt` (a Newton + Dekker-residual sqrt) and
        // `Fabs` (a `union` sign-bit clear) are HolyC in `lib/math.hc`.
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

/// The C library symbol the **heap intrinsics** lower to on the hosted (Darwin)
/// arm64 target. (This is the one remaining libc-backed group; it's a plain
/// name→symbol map, independent of the registry `all()` — `MAlloc`/`Free` are
/// `lib/mem.hc` intrinsics, not registry builtins. The freestanding targets ignore
/// this and emit an `mmap` bump allocator instead.)
pub fn libc_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "MAlloc" => "_malloc",
        "Free" => "_free",
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
