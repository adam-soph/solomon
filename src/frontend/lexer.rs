//! The HolyC lexer: turns source text into a stream of [`Token`]s.
//!
//! The lexer works over the raw bytes of the source. All syntactically
//! meaningful HolyC characters are ASCII. Non-ASCII bytes may appear only inside
//! string/char literals and comments, where they pass through untouched so the
//! resulting strings stay valid UTF-8.

use crate::token::{FileInfo, Keyword, Pos, Span, Token, TokenKind};
use std::fmt;

/// A lexical error with the location at which it occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub pos: Pos,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for LexError {}

type LResult<T> = Result<T, LexError>;

/// A pull-based source of tokens. Implemented by [`Lexer`] and by the
/// [`Preprocessor`](crate::preproc::Preprocessor), so the parser can run over
/// either while still pulling tokens lazily, one at a time.
pub trait TokenStream {
    fn next_token(&mut self) -> LResult<Token>;

    /// The source files seen, indexed by `Span::file`, for `_`-directory privacy.
    /// A bare lexer reports a single anonymous root file. The preprocessor
    /// overrides this with its real include table.
    fn source_files(&self) -> Vec<FileInfo> {
        vec![FileInfo::root()]
    }
}

impl TokenStream for Lexer {
    fn next_token(&mut self) -> LResult<Token> {
        // Delegate to the inherent method of the same name.
        Lexer::next_token(self)
    }
}

pub struct Lexer {
    /// The source bytes. Owned so a lexer outlives the `&str` it was built from;
    /// the preprocessor keeps a stack of these for `#include`d files.
    src: Vec<u8>,
    /// Current byte offset.
    idx: usize,
    /// Current 1-based line.
    line: u32,
    /// Current 1-based column.
    col: u32,
}

impl Lexer {
    pub fn new(src: &str) -> Self {
        Lexer {
            src: src.as_bytes().to_vec(),
            idx: 0,
            line: 1,
            col: 1,
        }
    }

    /// Tokenizes the whole input. Returns the token list, terminated by
    /// [`TokenKind::Eof`], or the first error encountered.
    pub fn tokenize(mut self) -> LResult<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    // ---- low-level cursor helpers ----

    fn peek(&self) -> Option<u8> {
        self.src.get(self.idx).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.idx + 1).copied()
    }

    fn peek3(&self) -> Option<u8> {
        self.src.get(self.idx + 2).copied()
    }

    /// Advances one byte, updating line/column tracking.
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.idx += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn pos(&self) -> Pos {
        Pos::new(self.line, self.col)
    }

    fn err<T>(&self, pos: Pos, msg: impl Into<String>) -> LResult<T> {
        Err(LexError {
            message: msg.into(),
            pos,
        })
    }

    // ---- main dispatch ----

    /// Produces the next token. The parser calls this on demand, so the full
    /// token list is never materialised in memory. Once the input is exhausted
    /// this returns [`TokenKind::Eof`] and keeps returning it on every further
    /// call, so calling past the end is safe.
    pub fn next_token(&mut self) -> LResult<Token> {
        self.skip_trivia()?;

        let start = self.idx;
        let pos = self.pos();

        let b = match self.peek() {
            None => return Ok(self.tok(TokenKind::Eof, start, pos)),
            Some(b) => b,
        };

        // Identifiers / keywords.
        if is_ident_start(b) {
            return Ok(self.lex_ident(start, pos));
        }

        // Numbers.
        if b.is_ascii_digit() {
            return self.lex_number(start, pos);
        }

        // String / char literals.
        if b == b'"' {
            return self.lex_string(start, pos);
        }
        if b == b'\'' {
            return self.lex_char(start, pos);
        }

        // Everything else is an operator or punctuation.
        self.lex_operator(start, pos)
    }

    fn tok(&self, kind: TokenKind, start: usize, pos: Pos) -> Token {
        Token::new(kind, Span::new(start, self.idx, pos))
    }

    // ---- trivia: whitespace and comments ----

    fn skip_trivia(&mut self) -> LResult<()> {
        loop {
            match self.peek() {
                Some(b) if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' => {
                    self.bump();
                }
                Some(b'/') if self.peek2() == Some(b'/') => {
                    // Line comment: consume to end of line.
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some(b'/') if self.peek2() == Some(b'*') => {
                    let pos = self.pos();
                    self.bump(); // /
                    self.bump(); // *
                    loop {
                        match self.peek() {
                            None => return self.err(pos, "unterminated block comment"),
                            Some(b'*') if self.peek2() == Some(b'/') => {
                                self.bump(); // *
                                self.bump(); // /
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    // ---- identifiers & keywords ----

    fn lex_ident(&mut self, start: usize, pos: Pos) -> Token {
        while let Some(b) = self.peek() {
            if is_ident_continue(b) {
                self.bump();
            } else {
                break;
            }
        }
        // Identifier bytes are ASCII, so this is always valid UTF-8.
        let text = std::str::from_utf8(&self.src[start..self.idx]).unwrap();
        let kind = match Keyword::from_str(text) {
            Some(kw) => TokenKind::Keyword(kw),
            None => TokenKind::Ident(text.to_string()),
        };
        self.tok(kind, start, pos)
    }

    // ---- numbers ----

    fn lex_number(&mut self, start: usize, pos: Pos) -> LResult<Token> {
        // Radix-prefixed integers: 0x.. (hex) and 0b.. (binary).
        if self.peek() == Some(b'0') {
            match self.peek2() {
                Some(b'x') | Some(b'X') => {
                    self.bump(); // 0
                    self.bump(); // x
                    return self.lex_radix_int(start, pos, 16, is_hex_digit);
                }
                Some(b'b') | Some(b'B') => {
                    self.bump(); // 0
                    self.bump(); // b
                    return self.lex_radix_int(start, pos, 2, is_bin_digit);
                }
                _ => {}
            }
        }

        // Decimal integer or float. Scan the integer part first.
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.bump();
        }

        let mut is_float = false;

        // Fractional part: a '.' followed by a digit. The digit is required so
        // that `1.foo` and `1..2` are not mis-lexed as floats.
        if self.peek() == Some(b'.') && matches!(self.peek2(), Some(b) if b.is_ascii_digit()) {
            is_float = true;
            self.bump(); // .
            while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                self.bump();
            }
        }

        // Exponent: e/E with optional sign and at least one digit.
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let mut k = 1;
            if matches!(self.peek2(), Some(b'+') | Some(b'-')) {
                k = 2;
            }
            let after = self.src.get(self.idx + k).copied();
            if matches!(after, Some(b) if b.is_ascii_digit()) {
                is_float = true;
                self.bump(); // e
                if k == 2 {
                    self.bump(); // sign
                }
                while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                    self.bump();
                }
            }
        }

        let text = std::str::from_utf8(&self.src[start..self.idx]).unwrap();
        if is_float {
            match text.parse::<f64>() {
                Ok(v) => Ok(self.tok(TokenKind::Float(v), start, pos)),
                Err(_) => self.err(pos, format!("invalid float literal `{text}`")),
            }
        } else if text.len() > 1 && text.starts_with('0') {
            // A leading `0` on a multi-digit integer means an octal literal, as
            // in C. `0x`/`0b` and floats were handled above, so a `0...` integer
            // reaching here is octal. A non-octal digit like `08`/`09` is an
            // error, also as in C.
            match parse_int_str(text, 8) {
                Some(v) => Ok(self.tok(TokenKind::Int(v), start, pos)),
                None => self.err(
                    pos,
                    format!("invalid octal literal `{text}` (digits must be 0-7)"),
                ),
            }
        } else {
            match parse_int_str(text, 10) {
                Some(v) => Ok(self.tok(TokenKind::Int(v), start, pos)),
                None => self.err(pos, format!("integer literal `{text}` out of range")),
            }
        }
    }

    fn lex_radix_int(
        &mut self,
        start: usize,
        pos: Pos,
        radix: u32,
        is_digit: fn(u8) -> bool,
    ) -> LResult<Token> {
        let digits_start = self.idx;
        while matches!(self.peek(), Some(b) if is_digit(b)) {
            self.bump();
        }
        if self.idx == digits_start {
            return self.err(pos, "missing digits after radix prefix");
        }
        let digits = std::str::from_utf8(&self.src[digits_start..self.idx]).unwrap();
        match parse_int_str(digits, radix) {
            Some(v) => Ok(self.tok(TokenKind::Int(v), start, pos)),
            None => self.err(pos, "integer literal out of range"),
        }
    }

    // ---- string literals ----

    fn lex_string(&mut self, start: usize, pos: Pos) -> LResult<Token> {
        self.bump(); // opening "
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return self.err(pos, "unterminated string literal");
                }
                Some(b'"') => {
                    self.bump();
                    return Ok(self.tok(TokenKind::Str(out), start, pos));
                }
                Some(b'\\') => {
                    self.bump();
                    let ch = self.lex_escape(pos)?;
                    out.push(ch);
                }
                Some(_) => {
                    // Pass bytes through, decoding any UTF-8 inside the literal.
                    out.push(self.bump_char());
                }
            }
        }
    }

    // ---- character constants ----

    /// HolyC character constants may hold several characters, packed
    /// little-endian into an I64. So `'A'` == 0x41 and `'AB'` == 0x4241.
    fn lex_char(&mut self, start: usize, pos: Pos) -> LResult<Token> {
        self.bump(); // opening '
        let mut value: i64 = 0;
        let mut count = 0u32;
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return self.err(pos, "unterminated character constant");
                }
                Some(b'\'') => {
                    self.bump();
                    if count == 0 {
                        return self.err(pos, "empty character constant");
                    }
                    return Ok(self.tok(TokenKind::Char(value), start, pos));
                }
                Some(b'\\') => {
                    self.bump();
                    let ch = self.lex_escape(pos)?;
                    self.pack_char(&mut value, &mut count, ch as u32, pos)?;
                }
                Some(_) => {
                    let ch = self.bump_char();
                    self.pack_char(&mut value, &mut count, ch as u32, pos)?;
                }
            }
        }
    }

    fn pack_char(&self, value: &mut i64, count: &mut u32, ch: u32, pos: Pos) -> LResult<()> {
        if *count >= 8 {
            return self.err(pos, "character constant too long (max 8 bytes)");
        }
        if ch > 0xFF {
            return self.err(pos, "character constant byte out of range");
        }
        *value |= (ch as i64) << (8 * *count);
        *count += 1;
        Ok(())
    }

    /// Consumes one escape sequence body and returns the resulting character.
    /// The backslash has already been eaten.
    fn lex_escape(&mut self, pos: Pos) -> LResult<char> {
        let b = match self.bump() {
            None => return self.err(pos, "unterminated escape sequence"),
            Some(b) => b,
        };
        Ok(match b {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'`' => '`',
            b'a' => '\u{07}', // bell
            b'b' => '\u{08}', // backspace
            b'f' => '\u{0C}', // form feed
            b'v' => '\u{0B}', // vertical tab
            b'x' => {
                // \xHH — one or two hex digits.
                let mut val: u32 = 0;
                let mut n = 0;
                while n < 2 && matches!(self.peek(), Some(c) if is_hex_digit(c)) {
                    let c = self.bump().unwrap();
                    val = val * 16 + hex_val(c);
                    n += 1;
                }
                if n == 0 {
                    return self.err(pos, "expected hex digits after `\\x`");
                }
                char::from_u32(val).unwrap_or('\u{FFFD}')
            }
            other => {
                return self.err(
                    pos,
                    format!("unknown escape sequence `\\{}`", other as char),
                );
            }
        })
    }

    /// Consumes one UTF-8 encoded character starting at the cursor and returns
    /// it. Assumes a byte is available. Invalid UTF-8 yields the replacement
    /// character.
    fn bump_char(&mut self) -> char {
        let first = self.peek().unwrap();
        let len = utf8_len(first);
        let chunk_end = (self.idx + len).min(self.src.len());
        let ch = std::str::from_utf8(&self.src[self.idx..chunk_end])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('\u{FFFD}');
        // Advance through the bytes consumed, keeping position tracking correct.
        for _ in 0..ch.len_utf8() {
            self.bump();
        }
        ch
    }

    // ---- operators & punctuation ----

    fn lex_operator(&mut self, start: usize, pos: Pos) -> LResult<Token> {
        use TokenKind::*;
        let b = self.peek().unwrap();
        let b2 = self.peek2();
        let b3 = self.peek3();

        // Maximal munch: try the longest operators first.
        let kind = match b {
            b'+' => match b2 {
                Some(b'+') => self.eat2(PlusPlus),
                Some(b'=') => self.eat2(PlusEq),
                _ => self.eat1(Plus),
            },
            b'-' => match b2 {
                Some(b'-') => self.eat2(MinusMinus),
                Some(b'=') => self.eat2(MinusEq),
                Some(b'>') => self.eat2(Arrow),
                _ => self.eat1(Minus),
            },
            b'*' => match b2 {
                Some(b'=') => self.eat2(StarEq),
                _ => self.eat1(Star),
            },
            b'/' => match b2 {
                Some(b'=') => self.eat2(SlashEq),
                _ => self.eat1(Slash),
            },
            b'%' => match b2 {
                Some(b'=') => self.eat2(PercentEq),
                _ => self.eat1(Percent),
            },
            b'=' => match b2 {
                Some(b'=') => self.eat2(EqEq),
                _ => self.eat1(Eq),
            },
            b'!' => match b2 {
                Some(b'=') => self.eat2(Ne),
                _ => self.eat1(Not),
            },
            b'<' => match (b2, b3) {
                (Some(b'<'), Some(b'=')) => self.eat3(ShlEq),
                (Some(b'<'), _) => self.eat2(Shl),
                (Some(b'='), _) => self.eat2(Le),
                _ => self.eat1(Lt),
            },
            b'>' => match (b2, b3) {
                (Some(b'>'), Some(b'=')) => self.eat3(ShrEq),
                (Some(b'>'), _) => self.eat2(Shr),
                (Some(b'='), _) => self.eat2(Ge),
                _ => self.eat1(Gt),
            },
            b'&' => match b2 {
                Some(b'&') => self.eat2(AndAnd),
                Some(b'=') => self.eat2(AmpEq),
                _ => self.eat1(Amp),
            },
            b'|' => match b2 {
                Some(b'|') => self.eat2(OrOr),
                Some(b'=') => self.eat2(PipeEq),
                _ => self.eat1(Pipe),
            },
            b'^' => match b2 {
                Some(b'=') => self.eat2(CaretEq),
                _ => self.eat1(Caret),
            },
            b'~' => self.eat1(Tilde),
            b'.' => match (b2, b3) {
                (Some(b'.'), Some(b'.')) => self.eat3(DotDotDot),
                _ => self.eat1(Dot),
            },
            b':' => match b2 {
                Some(b':') => self.eat2(ColonColon),
                Some(b'=') => self.eat2(ColonEq),
                _ => self.eat1(Colon),
            },
            b'?' => self.eat1(Question),
            b'(' => self.eat1(LParen),
            b')' => self.eat1(RParen),
            b'{' => self.eat1(LBrace),
            b'}' => self.eat1(RBrace),
            b'[' => self.eat1(LBracket),
            b']' => self.eat1(RBracket),
            b',' => self.eat1(Comma),
            b';' => self.eat1(Semicolon),
            b'@' => self.eat1(At),
            b'#' => self.eat1(Hash),
            b'`' => self.eat1(Backtick),
            other => {
                return self.err(pos, format!("unexpected character `{}`", other as char));
            }
        };
        Ok(self.tok(kind, start, pos))
    }

    fn eat1(&mut self, kind: TokenKind) -> TokenKind {
        self.bump();
        kind
    }

    fn eat2(&mut self, kind: TokenKind) -> TokenKind {
        self.bump();
        self.bump();
        kind
    }

    fn eat3(&mut self, kind: TokenKind) -> TokenKind {
        self.bump();
        self.bump();
        self.bump();
        kind
    }
}

/// Convenience wrapper: tokenize a string slice.
pub fn tokenize(src: &str) -> LResult<Vec<Token>> {
    Lexer::new(src).tokenize()
}

// ---- free helpers ----

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn is_bin_digit(b: u8) -> bool {
    b == b'0' || b == b'1'
}

fn hex_val(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => (b - b'0') as u32,
        b'a'..=b'f' => (b - b'a' + 10) as u32,
        b'A'..=b'F' => (b - b'A' + 10) as u32,
        _ => 0,
    }
}

/// Number of bytes in a UTF-8 sequence given its leading byte.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Parses an integer in the given radix into an i64. HolyC integers are 64-bit,
/// so any bit pattern that fits in 64 bits is accepted: 0xFFFFFFFFFFFFFFFF is
/// valid and wraps to -1. This matches C-like signed/unsigned reinterpretation.
fn parse_int_str(s: &str, radix: u32) -> Option<i64> {
    match i64::from_str_radix(s, radix) {
        Ok(v) => Some(v),
        // Out of i64 range, but maybe a valid u64 bit pattern (e.g. high bit set).
        Err(_) => u64::from_str_radix(s, radix).ok().map(|v| v as i64),
    }
}
