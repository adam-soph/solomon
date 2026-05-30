//! Intrinsic functions solomon provides without a user definition.
//!
//! This is the single source of truth shared by semantic analysis (which
//! registers their signatures so calls type-check) and the interpreter (which
//! implements their behaviour). Adding an intrinsic means adding an entry here
//! plus a behaviour arm in `backend::interp`. These will be superseded by the
//! real core / standard library when it lands.

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
    ]
}

/// Whether `name` is a builtin function.
pub fn is_builtin(name: &str) -> bool {
    all().iter().any(|b| b.name == name)
}
