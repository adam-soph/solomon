//! Token definitions for the HolyC lexer.

use std::fmt;

/// A position in the source file. Both `line` and `col` are 1-based, which is
/// what humans expect to see in error messages.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub fn new(line: u32, col: u32) -> Self {
        Pos { line, col }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

impl fmt::Debug for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// A half-open span `[start, end)` of byte offsets into the source, paired with
/// the start position for diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub pos: Pos,
}

impl Span {
    pub fn new(start: usize, end: usize, pos: Pos) -> Self {
        Span { start, end, pos }
    }

    /// A placeholder span (all zeroes). Useful in tests that build AST nodes by
    /// hand, since AST equality ignores spans.
    pub fn dummy() -> Self {
        Span::new(0, 0, Pos::new(0, 0))
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}@{}", self.start, self.end, self.pos)
    }
}

/// Generate the `Keyword` enum together with its string mapping (`from_str` /
/// `as_str`) and the `is_type` predicate from a single table, so the four
/// pieces of information can't drift apart. Each row is
/// `Variant => "spelling", <is a built-in type>`.
macro_rules! keywords {
    ($($variant:ident => $spelling:literal, $is_type:literal);+ $(;)?) => {
        /// HolyC keywords: reserved words and built-in type names recognised
        /// directly by the lexer so the parser never string-compares identifiers.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Keyword {
            $($variant),+
        }

        impl Keyword {
            /// Map an identifier to its keyword, if it is one. Case-sensitive.
            pub fn from_str(s: &str) -> Option<Keyword> {
                match s {
                    $($spelling => Some(Keyword::$variant),)+
                    _ => None,
                }
            }

            /// The canonical spelling of this keyword.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Keyword::$variant => $spelling),+
                }
            }

            /// Whether this keyword names a built-in type (lets the parser tell
            /// declarations from expression statements).
            pub fn is_type(self) -> bool {
                match self {
                    $(Keyword::$variant => $is_type),+
                }
            }

            /// Every keyword, for exhaustive iteration (e.g. consistency tests).
            pub const ALL: &'static [Keyword] = &[$(Keyword::$variant),+];
        }
    };
}

keywords! {
    // Built-in types. HolyC's default integer is I64 and there is no F32.
    U0 => "U0", true;
    I8 => "I8", true;
    U8 => "U8", true;
    I16 => "I16", true;
    U16 => "U16", true;
    I32 => "I32", true;
    U32 => "U32", true;
    I64 => "I64", true;
    U64 => "U64", true;
    F64 => "F64", true;
    Bool => "Bool", true;

    // Control flow.
    If => "if", false;
    Else => "else", false;
    While => "while", false;
    Do => "do", false;
    For => "for", false;
    Switch => "switch", false;
    Case => "case", false;
    Default => "default", false;
    Break => "break", false;
    Continue => "continue", false;
    Return => "return", false;
    Goto => "goto", false;

    // Aggregates / declarations.
    Class => "class", false;
    Union => "union", false;
    Typedef => "typedef", false;
    Public => "public", false;
    Extern => "extern", false;
    Import => "import", false;
    Reg => "reg", false;
    NoReg => "noreg", false;
    Lastclass => "lastclass", false;
    Sizeof => "sizeof", false;
    Offset => "offset", false;
    NoWarn => "no_warn", false;

    // Exceptions.
    Try => "try", false;
    Catch => "catch", false;
    Throw => "throw", false;

    // Switch-range markers (`start:` ... `end:` inside a `switch [...]`).
    Start => "start", false;
    End => "end", false;

    // Inline assembly.
    Asm => "asm", false;
}

/// The kind of a token. Literals carry their decoded value; operators and
/// punctuation each get their own variant so the parser can match on them
/// without re-inspecting the source text.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // ---- Literals & names ----
    /// Integer literal (decimal, `0x` hex, or `0b` binary), already parsed.
    Int(i64),
    /// Floating-point literal (HolyC only has F64).
    Float(f64),
    /// String literal with escapes already resolved.
    Str(String),
    /// Character constant. HolyC packs up to 8 chars little-endian into an I64,
    /// e.g. `'AB'` == 0x4241, so the value is stored as an i64.
    Char(i64),
    /// Identifier (not a keyword).
    Ident(String),
    /// A reserved word or built-in type name.
    Keyword(Keyword),

    // ---- Arithmetic ----
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %

    // ---- Assignment (compound and simple) ----
    Eq,        // =
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    AmpEq,     // &=
    PipeEq,    // |=
    CaretEq,   // ^=
    ShlEq,     // <<=
    ShrEq,     // >>=

    // ---- Increment / decrement ----
    PlusPlus,   // ++
    MinusMinus, // --

    // ---- Comparison ----
    EqEq, // ==
    Ne,   // !=
    Lt,   // <
    Gt,   // >
    Le,   // <=
    Ge,   // >=

    // ---- Logical ----
    AndAnd, // &&
    OrOr,   // ||
    Not,    // !

    // ---- Bitwise ----
    Amp,   // &
    Pipe,  // |
    Caret, // ^
    Tilde, // ~
    Shl,   // <<
    Shr,   // >>

    // ---- Punctuation ----
    LParen,     // (
    RParen,     // )
    LBrace,     // {
    RBrace,     // }
    LBracket,   // [
    RBracket,   // ]
    Comma,      // ,
    Semicolon,  // ;
    Dot,        // .
    Arrow,      // ->
    Question,   // ?
    Colon,      // :
    ColonColon, // ::
    DotDotDot,  // ...   (varargs / case ranges)
    At,         // @
    Hash,       // #     (preprocessor directives: #include, #define, ...)
    Backtick,   // `

    /// End of input. Always the last token.
    Eof,
}

/// A lexed token: its kind plus where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}
