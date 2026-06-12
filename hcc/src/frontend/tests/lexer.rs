//! Unit tests for the lexer: integer/float/char/string decoding, maximal-munch
//! operators, keyword recognition, comment skipping, and the free numeric helpers.
//! Reaches the module's private free functions (`parse_int_str`, `utf8_len`, the
//! `is_ident_*` predicates) via `use super::*`.

use super::*;

/// The `TokenKind`s of `src`, with the trailing `Eof` stripped (and asserted).
fn kinds(src: &str) -> Vec<TokenKind> {
    let mut ks: Vec<TokenKind> = tokenize(src)
        .expect("lex")
        .into_iter()
        .map(|t| t.kind)
        .collect();
    assert_eq!(ks.pop(), Some(TokenKind::Eof), "stream must end in Eof");
    ks
}

/// The first token kind of `src`.
fn first(src: &str) -> TokenKind {
    kinds(src).into_iter().next().expect("at least one token")
}

#[test]
fn integer_radixes_and_octal() {
    assert_eq!(first("0"), TokenKind::Int(0));
    assert_eq!(first("42"), TokenKind::Int(42));
    assert_eq!(first("0xFF"), TokenKind::Int(255));
    assert_eq!(first("0b1010"), TokenKind::Int(10));
    assert_eq!(first("0755"), TokenKind::Int(0o755)); // leading 0 ⇒ octal, as in C
}

#[test]
fn integer_wraps_through_u64_bit_pattern() {
    // 0xFFFF_FFFF_FFFF_FFFF is out of i64 range but a valid u64 bit pattern ⇒ -1.
    assert_eq!(first("0xFFFFFFFFFFFFFFFF"), TokenKind::Int(-1));
}

#[test]
fn floats_with_fraction_and_exponent() {
    assert_eq!(first("0.5"), TokenKind::Float(0.5));
    assert_eq!(first("1e3"), TokenKind::Float(1000.0));
    assert_eq!(first("2.5e-1"), TokenKind::Float(0.25));
    // `1.foo` is NOT a float: the fractional '.' requires a trailing digit.
    assert_eq!(
        kinds("1.foo"),
        vec![
            TokenKind::Int(1),
            TokenKind::Dot,
            TokenKind::Ident("foo".into()),
        ],
    );
}

#[test]
fn char_constants_pack_little_endian() {
    assert_eq!(first("'A'"), TokenKind::Char(0x41));
    assert_eq!(first("'AB'"), TokenKind::Char(0x4241)); // 'A' | ('B' << 8)
    assert_eq!(first(r"'\n'"), TokenKind::Char(0x0A));
}

#[test]
fn string_escapes_resolved() {
    assert_eq!(first(r#""a\tb\n""#), TokenKind::Str("a\tb\n".into()));
    assert_eq!(first(r#""\x41""#), TokenKind::Str("A".into()));
}

#[test]
fn operators_maximal_munch() {
    use TokenKind::*;
    assert_eq!(
        kinds("<<= >> -> := ... :: ++ == != <= >="),
        vec![
            ShlEq, Shr, Arrow, ColonEq, DotDotDot, ColonColon, PlusPlus, EqEq, Ne, Le, Ge
        ],
    );
}

#[test]
fn keywords_vs_identifiers() {
    assert_eq!(first("I64"), TokenKind::Keyword(Keyword::I64));
    assert_eq!(first("class"), TokenKind::Keyword(Keyword::Class));
    assert_eq!(first("foo_bar"), TokenKind::Ident("foo_bar".into()));
    assert_eq!(first("_x9"), TokenKind::Ident("_x9".into()));
}

#[test]
fn comments_are_trivia() {
    assert_eq!(
        kinds("1 /* block\ncomment */ + 2 // trailing"),
        vec![TokenKind::Int(1), TokenKind::Plus, TokenKind::Int(2)],
    );
}

#[test]
fn unterminated_constructs_error() {
    assert!(tokenize("\"no end").is_err()); // string
    assert!(tokenize("'").is_err()); // char
    assert!(tokenize("/* open").is_err()); // block comment
    assert!(tokenize("0x").is_err()); // radix prefix with no digits
}

// ---- free helpers ----

#[test]
fn parse_int_str_handles_radix_and_wraparound() {
    assert_eq!(parse_int_str("ff", 16), Some(255));
    assert_eq!(parse_int_str("1010", 2), Some(10));
    assert_eq!(parse_int_str("FFFFFFFFFFFFFFFF", 16), Some(-1)); // u64 bit pattern
    assert_eq!(parse_int_str("zzz", 16), None);
}

#[test]
fn utf8_len_classifies_leading_byte() {
    assert_eq!(utf8_len(b'a'), 1);
    assert_eq!(utf8_len(0xC3), 2); // 2-byte sequence leader
    assert_eq!(utf8_len(0xE2), 3); // 3-byte
    assert_eq!(utf8_len(0xF0), 4); // 4-byte
}

#[test]
fn ident_byte_classes() {
    assert!(is_ident_start(b'_'));
    assert!(is_ident_start(b'a'));
    assert!(!is_ident_start(b'9'));
    assert!(is_ident_continue(b'9'));
    assert!(!is_ident_continue(b'-'));
}

#[test]
fn keyword_table_round_trips() {
    for &kw in Keyword::ALL {
        assert_eq!(Keyword::from_str(kw.as_str()), Some(kw), "{kw:?}");
    }
    assert_eq!(Keyword::from_str("notakeyword"), None);
}
