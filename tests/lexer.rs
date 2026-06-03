//! Integration tests for the HolyC lexer.

use solomon::lexer::tokenize;
use solomon::token::{Keyword, TokenKind};

/// Tokenize and strip spans, dropping the trailing Eof for concise assertions.
fn kinds(src: &str) -> Vec<TokenKind> {
    let mut toks: Vec<TokenKind> = tokenize(src).unwrap().into_iter().map(|t| t.kind).collect();
    assert_eq!(toks.pop(), Some(TokenKind::Eof), "must end with Eof");
    toks
}

#[test]
fn keyword_tables_are_consistent() {
    use solomon::ast::Type;
    use solomon::token::Keyword;
    for &kw in Keyword::ALL {
        // from_str and as_str are inverses.
        assert_eq!(
            Keyword::from_str(kw.as_str()),
            Some(kw),
            "round-trip failed for {kw:?}"
        );
        // is_type agrees with Type::from_keyword.
        assert_eq!(
            kw.is_type(),
            Type::from_keyword(kw).is_some(),
            "is_type / from_keyword disagree for {kw:?}"
        );
    }
}

#[test]
fn empty_input_is_just_eof() {
    let toks = tokenize("").unwrap();
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, TokenKind::Eof);
}

#[test]
fn whitespace_and_comments_are_skipped() {
    let src = "  \t\n // line comment\n /* block\n comment */ 42";
    assert_eq!(kinds(src), vec![TokenKind::Int(42)]);
}

#[test]
fn identifiers_and_keywords() {
    use Keyword::*;
    let toks = kinds("U0 main foo_bar I64 while While");
    assert_eq!(
        toks,
        vec![
            TokenKind::Keyword(U0),
            TokenKind::Ident("main".into()),
            TokenKind::Ident("foo_bar".into()),
            TokenKind::Keyword(I64),
            TokenKind::Keyword(While),
            // `While` is case-sensitively distinct from the keyword `while`.
            TokenKind::Ident("While".into()),
        ]
    );
}

#[test]
fn decimal_hex_binary_integers() {
    assert_eq!(
        kinds("0 42 255 0xFF 0Xff 0b1010 0B11"),
        vec![
            TokenKind::Int(0),
            TokenKind::Int(42),
            TokenKind::Int(255),
            TokenKind::Int(255),
            TokenKind::Int(255),
            TokenKind::Int(10),
            TokenKind::Int(3),
        ]
    );
}

#[test]
fn octal_integers() {
    // A leading `0` on a multi-digit integer is octal (C semantics); a bare `0` and a
    // float starting with `0` are unaffected.
    assert_eq!(
        kinds("0 010 0644 0755 0.5"),
        vec![
            TokenKind::Int(0),
            TokenKind::Int(8),
            TokenKind::Int(420),
            TokenKind::Int(493),
            TokenKind::Float(0.5),
        ]
    );
}

#[test]
fn invalid_octal_digit_is_an_error() {
    assert!(solomon::tokenize("08").is_err());
}

#[test]
fn high_bit_hex_wraps_to_negative() {
    // 0xFFFFFFFFFFFFFFFF is a valid 64-bit pattern == -1 as i64.
    assert_eq!(kinds("0xFFFFFFFFFFFFFFFF"), vec![TokenKind::Int(-1)]);
}

#[test]
fn floats() {
    assert_eq!(
        kinds("3.14 0.5 1e3 2.5e-2 10E+1"),
        vec![
            TokenKind::Float(3.14),
            TokenKind::Float(0.5),
            TokenKind::Float(1e3),
            TokenKind::Float(2.5e-2),
            TokenKind::Float(10e1),
        ]
    );
}

#[test]
fn dot_after_int_is_not_a_float() {
    // `1.foo` => Int(1) Dot Ident; `1..` would also not be a float.
    assert_eq!(
        kinds("1.foo"),
        vec![
            TokenKind::Int(1),
            TokenKind::Dot,
            TokenKind::Ident("foo".into()),
        ]
    );
}

#[test]
fn strings_with_escapes() {
    assert_eq!(
        kinds(r#""Hello, World!\n""#),
        vec![TokenKind::Str("Hello, World!\n".into())]
    );
    assert_eq!(
        kinds(r#""tab\there\x41\\end""#),
        vec![TokenKind::Str("tab\there\x41\\end".into())]
    );
}

#[test]
fn char_constants_pack_little_endian() {
    assert_eq!(kinds("'A'"), vec![TokenKind::Char(0x41)]);
    // 'AB' => 'A' | 'B'<<8 == 0x4241.
    assert_eq!(kinds("'AB'"), vec![TokenKind::Char(0x4241)]);
    assert_eq!(kinds(r"'\n'"), vec![TokenKind::Char(0x0A)]);
}

#[test]
fn maximal_munch_operators() {
    use TokenKind::*;
    assert_eq!(
        kinds("<<= >>= ... :: -> ++ -- == != <= >= && || << >> += <"),
        vec![
            ShlEq, ShrEq, DotDotDot, ColonColon, Arrow, PlusPlus, MinusMinus, EqEq, Ne, Le, Ge,
            AndAnd, OrOr, Shl, Shr, PlusEq, Lt,
        ]
    );
}

#[test]
fn punctuation_and_holyc_specials() {
    use TokenKind::*;
    assert_eq!(
        kinds("( ) { } [ ] , ; @ # `"),
        vec![
            LParen, RParen, LBrace, RBrace, LBracket, RBracket, Comma, Semicolon, At, Hash,
            Backtick,
        ]
    );
}

#[test]
fn a_small_program() {
    use TokenKind::*;
    use solomon::token::Keyword as K;
    let src = r#"U0 Main() {
    I64 i;
    for (i = 0; i < 10; i++)
        "Hello %d\n", i;
}"#;
    assert_eq!(
        kinds(src),
        vec![
            Keyword(K::U0),
            Ident("Main".into()),
            LParen,
            RParen,
            LBrace,
            Keyword(K::I64),
            Ident("i".into()),
            Semicolon,
            Keyword(K::For),
            LParen,
            Ident("i".into()),
            Eq,
            Int(0),
            Semicolon,
            Ident("i".into()),
            Lt,
            Int(10),
            Semicolon,
            Ident("i".into()),
            PlusPlus,
            RParen,
            Str("Hello %d\n".into()),
            Comma,
            Ident("i".into()),
            Semicolon,
            RBrace,
        ]
    );
}

#[test]
fn tracks_line_and_column() {
    let toks = tokenize("a\n  b").unwrap();
    assert_eq!((toks[0].span.pos.line, toks[0].span.pos.col), (1, 1));
    assert_eq!((toks[1].span.pos.line, toks[1].span.pos.col), (2, 3));
}

#[test]
fn errors_report_position() {
    let e = tokenize("  \"unterminated").unwrap_err();
    assert_eq!(e.pos.line, 1);
    assert!(e.message.contains("unterminated string"));

    assert!(tokenize("/* nope").is_err());
    assert!(tokenize("''").is_err()); // empty char constant
    assert!(tokenize("0x").is_err()); // no digits after prefix
    assert!(tokenize(r#""\q""#).is_err()); // unknown escape
}
