//! Recursive-descent parser for HolyC.
//!
//! The parser is generic over a `TokenStream` and pulls tokens lazily through it.
//! In practice that stream is a `Preprocessor` wrapping a [`Lexer`], so macros and
//! `#include`s splice in transparently. Only a tiny look-ahead buffer (a couple of
//! tokens) is kept, so the complete token stream is never held in memory at once.
//!
//! Parsing is two-pass: a first sweep hoists `class`/`union` names so a type can be
//! used before it is defined, then the real parse runs.
//!
//! Every node carries a [`Span`] running from the start of its first token to the
//! end of its last. The parser tracks the end of the most recently consumed token
//! in `prev_end`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use crate::ast::*;
use crate::lexer::{LexError, Lexer, TokenStream};
use crate::preproc::Preprocessor;
use crate::token::{Keyword, Pos, Span, Token, TokenKind};

/// A parse error: a message plus the source position where it was detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub pos: Pos,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            message: e.message,
            pos: e.pos,
        }
    }
}

type PResult<T> = Result<T, ParseError>;

/// The start of a node, captured before parsing it. It is later paired with the end
/// of the last consumed token to form a [`Span`].
#[derive(Clone, Copy)]
struct Mark {
    start: usize,
    pos: Pos,
    /// The source file (`Span::file`) of the token at the mark. AST nodes built from
    /// this mark carry their origin, which `_`-directory privacy checks rely on.
    file: u32,
}

// The generic-template types (`GenericFn`, `GenericClass`, `TypePattern`) live in
// `ast.rs`; they ride along on `Program::generics` for the `mono` pass. The parser
// uses them via the `use crate::ast::*` glob above.

/// Registry of generic templates the parser builds while parsing.
///
/// It records template *names* — so a `Name<...>` use is recognized as generic
/// rather than a less-than — and their parsed-once AST bodies. Instantiation,
/// worklists, dedup, and type-directed inference all happen later in the
/// [`mono`](crate::mono) pass, which takes these templates off
/// [`Program::generics`].
#[derive(Default)]
struct Generics {
    /// Generic `class`/`union` templates, by name, registered at their declaration
    /// (define-before-use, like `typedef` aliases).
    classes: HashMap<String, GenericClass>,
    /// Generic function templates, by name (define-before-use).
    fns: HashMap<String, GenericFn>,
}

impl Generics {
    fn new() -> Self {
        Generics::default()
    }
}

pub struct Parser<S: TokenStream> {
    stream: S,
    /// Look-ahead buffer. Tokens are pulled from the stream only as the parser
    /// peeks past what it has already seen.
    buf: VecDeque<Token>,
    /// Byte offset just past the most recently consumed token, used as the end
    /// of node spans.
    prev_end: usize,
    /// Names known to be types: lexer built-ins, plus `class`/`union` names hoisted
    /// ahead of time and any seen while parsing. This lets the parser tell `Foo x;`
    /// (a declaration) from `Foo * x` (a multiplication) regardless of definition
    /// order.
    known_types: HashSet<String>,
    /// `typedef` aliases, mapping a name to the type it stands for. Resolved at parse
    /// time, so an alias never reaches the AST as a `Named` type. Aliases must be
    /// defined before use (the C rule).
    type_aliases: HashMap<String, Type>,
    /// Synthetic type definitions produced while parsing, such as an inline or
    /// anonymous `union` embedded in a class. They are injected as top-level items
    /// before the item that referenced them.
    pending_types: Vec<Stmt>,
    /// Counter for naming anonymous embedded unions (`$anonN`).
    anon_counter: u32,
    /// Canonical names of the anonymous `class`/`union` types already synthesized
    /// (`$Cls…`/`$ClsU…`). Two identical anonymous aggregates share one synthetic
    /// definition, so this guards against emitting a duplicate top-level type.
    anon_types: HashSet<String>,
    /// The generic-template registry: names plus parsed-once template ASTs. See
    /// [`Generics`]. Instantiation happens later, in the [`mono`](crate::mono) pass.
    generics: Generics,
    /// Current recursion depth. Every nested expression and statement funnels through
    /// `parse_unary`/`parse_stmt`, which bump this so pathologically deep input fails
    /// with a `ParseError` rather than overflowing the stack.
    depth: u32,
    /// Type parameters of the generic template currently being parsed, or `None`
    /// outside one. While set, `parse_base_type` resolves a parameter name to
    /// `Type::Param`, keeping a template body's parameters symbolic for `mono` to
    /// substitute.
    template_params: Option<Vec<String>>,
    /// Whether the statement about to be parsed is a top-level item (directly under
    /// [`parse_program`]), not a function-local one. `parse_program` sets it before each
    /// item and `parse_stmt` captures-then-clears it, so a bare top-level
    /// function-pointer declarator (`I64 (*BinOp)(I64, I64);`) can be recognised as a
    /// *type* alias while a local one of the same shape stays a variable.
    top_level: bool,
}

/// Maximum expression/statement nesting depth before the parser bails out.
const MAX_PARSE_DEPTH: u32 = 256;

impl<S: TokenStream> Parser<S> {
    /// Build a parser that draws tokens from `stream` (a [`Lexer`] or a
    /// [`Preprocessor`]).
    pub fn new(stream: S) -> Self {
        Parser::with_known_types(stream, HashSet::new())
    }

    /// Build a parser with a pre-seeded set of known type names (used by the
    /// type-hoisting pre-pass).
    pub fn with_known_types(stream: S, known_types: HashSet<String>) -> Self {
        Parser {
            stream,
            buf: VecDeque::new(),
            prev_end: 0,
            known_types,
            type_aliases: HashMap::new(),
            pending_types: Vec::new(),
            anon_counter: 0,
            anon_types: HashSet::new(),
            generics: Generics::new(),
            depth: 0,
            template_params: None,
            top_level: false,
        }
    }

    /// Enter a recursive parse frame, erroring if nesting is too deep. Paired with a
    /// `self.depth -= 1` on the success path. An error aborts the whole parse, so a
    /// leftover count is harmless.
    fn enter_recursion(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return self.err("input nested too deeply");
        }
        Ok(())
    }

    /// Parse a whole translation unit.
    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof)? {
            self.top_level = true;
            let stmt = self.parse_stmt()?;
            // Synthetic types (embedded unions) defined while parsing this item are
            // emitted first, so they're laid out and registered before their use.
            items.append(&mut self.pending_types);
            items.push(stmt);
        }
        // The accumulated include table (indexed by `Span::file`) rides along for
        // `_`-directory privacy checks in sema.
        let files = self.stream.source_files();
        // Carry the generic templates for the `mono` pass. Until it runs, `items`
        // still holds the deferred `Type::Generic`/`GenericCall`/`Type::Tuple`/
        // `ShortDecl` nodes it instantiates.
        let generics = GenericTemplates {
            classes: std::mem::take(&mut self.generics.classes),
            fns: std::mem::take(&mut self.generics.fns),
        };
        Ok(Program {
            items,
            files,
            generics,
        })
    }

    // ---- token buffer / look-ahead ----

    /// Make sure the buffer holds at least `n + 1` tokens (or has reached Eof).
    fn ensure(&mut self, n: usize) -> PResult<()> {
        while self.buf.len() <= n {
            // Stop pulling once Eof is buffered; the lexer would otherwise keep
            // handing back Eof forever.
            if matches!(self.buf.back().map(|t| &t.kind), Some(TokenKind::Eof)) {
                break;
            }
            let tok = self.stream.next_token()?;
            self.buf.push_back(tok);
        }
        Ok(())
    }

    /// Look at the current token without consuming it.
    fn peek(&mut self) -> PResult<&Token> {
        self.peek_n(0)
    }

    /// Look `n` tokens ahead (0 == current). Reads past Eof return the Eof token.
    fn peek_n(&mut self, n: usize) -> PResult<&Token> {
        self.ensure(n)?;
        let idx = n.min(self.buf.len() - 1);
        Ok(&self.buf[idx])
    }

    /// Consume and return the current token. At Eof this returns Eof without
    /// consuming, so it is safe to over-call.
    fn advance(&mut self) -> PResult<Token> {
        self.ensure(0)?;
        if matches!(self.buf.front().map(|t| &t.kind), Some(TokenKind::Eof)) {
            let eof = self.buf.front().unwrap().clone();
            self.prev_end = eof.span.end;
            return Ok(eof);
        }
        let tok = self.buf.pop_front().unwrap();
        self.prev_end = tok.span.end;
        Ok(tok)
    }

    /// A clone of the current token kind. Handy when matching on it while still
    /// calling `&mut self` methods in the arms.
    fn peek_kind(&mut self) -> PResult<TokenKind> {
        Ok(self.peek()?.kind.clone())
    }

    fn cur_pos(&mut self) -> Pos {
        // The buffer always has at least the Eof token available.
        self.peek().map(|t| t.span.pos).unwrap_or(Pos::new(0, 0))
    }

    /// Capture the start of a node at the current token.
    fn mark(&mut self) -> PResult<Mark> {
        let t = self.peek()?;
        Ok(Mark {
            start: t.span.start,
            pos: t.span.pos,
            file: t.span.file,
        })
    }

    /// Build the span running from `m` to the end of the last consumed token.
    fn finish(&self, m: Mark) -> Span {
        let mut span = Span::new(m.start, self.prev_end, m.pos);
        span.file = m.file;
        span
    }

    fn ex(&self, kind: ExprKind, m: Mark) -> Expr {
        Expr::new(kind, self.finish(m))
    }

    fn st(&self, kind: StmtKind, m: Mark) -> Stmt {
        Stmt::new(kind, self.finish(m))
    }

    fn at(&mut self, k: &TokenKind) -> PResult<bool> {
        Ok(&self.peek()?.kind == k)
    }

    /// Consume the current token if it equals `k`; report whether it did.
    fn eat(&mut self, k: &TokenKind) -> PResult<bool> {
        if self.at(k)? {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Consume a contextual word — an identifier with the given spelling — if present.
    /// Used for the `is`/`not` in `if type (T is U)`; they are not reserved keywords, so
    /// they only mean anything in that position and stay usable as ordinary identifiers.
    fn eat_word(&mut self, word: &str) -> PResult<bool> {
        if matches!(self.peek_kind()?, TokenKind::Ident(ref s) if s == word) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, k: &TokenKind, what: &str) -> PResult<Token> {
        if self.at(k)? {
            self.advance()
        } else {
            let t = self.peek()?;
            Err(ParseError {
                message: format!("expected {what}, found {:?}", t.kind),
                pos: t.span.pos,
            })
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        let t = self.advance()?;
        match t.kind {
            TokenKind::Ident(s) => Ok(s),
            other => Err(ParseError {
                message: format!("expected identifier, found {other:?}"),
                pos: t.span.pos,
            }),
        }
    }

    fn err<T>(&mut self, msg: impl Into<String>) -> PResult<T> {
        let pos = self.cur_pos();
        Err(ParseError {
            message: msg.into(),
            pos,
        })
    }

    fn err_at<T>(&self, pos: Pos, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            message: msg.into(),
            pos,
        })
    }

    // ---- type detection ----

    fn kind_is_type_start(&self, k: &TokenKind) -> bool {
        match k {
            // `class`/`union` introduce an anonymous aggregate type (`class { … }`)
            // in type position. The statement-level `class Name { … }` definition is
            // dispatched before this in `parse_stmt_inner`, so it isn't affected.
            TokenKind::Keyword(kw) => kw.is_type() || matches!(kw, Keyword::Class | Keyword::Union),
            // A class/union name, or (inside a generic template) a type parameter, so
            // `T x;` or `V zero;` in a template body parses as a declaration.
            TokenKind::Ident(s) => {
                self.known_types.contains(s)
                    || self.template_params.as_ref().is_some_and(|p| p.contains(s))
            }
            _ => false,
        }
    }

    fn is_type_start(&mut self) -> PResult<bool> {
        let k = self.peek_kind()?;
        if !matches!(k, TokenKind::LParen) {
            return Ok(self.kind_is_type_start(&k));
        }
        // A `(` begins a tuple type `(T, …)`, hence a declaration, only when a type
        // starts right after it AND a top-level `,` precedes the matching `)`. That
        // combination distinguishes it from `(expr)` and a cast `(T)expr`.
        let k1 = self.peek_n(1)?.kind.clone();
        if !self.kind_is_type_start(&k1) {
            return Ok(false);
        }
        let mut depth = 0i32;
        let mut i = 1;
        loop {
            match self.peek_n(i)?.kind.clone() {
                // Track `{`/`}` too, so a comma inside an anonymous aggregate element
                // (`(class { I64 a, b; }, I64)`) isn't mistaken for the tuple's
                // top-level separator.
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        return Ok(false); // closed with no top-level comma, so not a tuple
                    }
                    depth -= 1;
                }
                TokenKind::Comma if depth == 0 => return Ok(true),
                TokenKind::Eof => return Ok(false),
                _ => {}
            }
            i += 1;
            if i > 512 {
                return Ok(false);
            }
        }
    }

    /// Parse a base type name (`I64`, a class name, ...). Pointers and arrays
    /// are applied by [`Self::parse_declarator`], not here.
    fn parse_base_type(&mut self) -> PResult<Type> {
        // A `(` in type position introduces a tuple type `(T1, …, Tn)`.
        if self.at(&TokenKind::LParen)? {
            return self.parse_tuple_type();
        }
        // `class { … }` / `union { … }` in type position is an anonymous aggregate.
        if matches!(
            self.peek_kind()?,
            TokenKind::Keyword(Keyword::Class | Keyword::Union)
        ) && matches!(self.peek_n(1)?.kind, TokenKind::LBrace)
        {
            return self.parse_anon_aggregate();
        }
        let t = self.advance()?;
        match t.kind {
            TokenKind::Keyword(kw) => Type::from_keyword(kw).ok_or_else(|| ParseError {
                message: format!("`{}` is not a type", kw.as_str()),
                pos: t.span.pos,
            }),
            TokenKind::Ident(s) => {
                // Inside a generic template, a parameter name stays symbolic.
                if let Some(params) = &self.template_params {
                    if params.contains(&s) {
                        return Ok(Type::Param(s));
                    }
                }
                // A `typedef` alias resolves to its target type.
                if let Some(ty) = self.type_aliases.get(&s) {
                    return Ok(ty.clone());
                }
                // A generic-class use `Name<args>` is deferred to a `Type::Generic`;
                // the `mono` pass instantiates it. Keeping all monomorphization in that
                // one type-directed place is deliberate.
                if self.generics.classes.contains_key(&s) && self.at(&TokenKind::Lt)? {
                    let params = self.generics.classes[&s].params.clone();
                    let args = self.parse_generic_args(&params)?;
                    return Ok(Type::Generic(s, args));
                }
                // Any other identifier is a class/union name.
                Ok(Type::Named(s))
            }
            other => Err(ParseError {
                message: format!("expected a type, found {other:?}"),
                pos: t.span.pos,
            }),
        }
    }

    /// Parse an anonymous `class { … }` / `union { … }` type in type position and
    /// return a reference to it as a `Type::Named`.
    ///
    /// An anonymous aggregate is **structural**: its name is derived from its field
    /// signature ([`anon_aggregate_name`]), so two identical anonymous aggregates
    /// anywhere name the same synthetic type. The first occurrence synthesizes a
    /// top-level `class`/`union` definition (pushed to `pending_types`, flushed before
    /// the referencing item); later occurrences reuse it. This mirrors how tuple types
    /// are interned, and lets anonymous aggregates appear at any type position with no
    /// per-site handling.
    fn parse_anon_aggregate(&mut self) -> PResult<Type> {
        let dm = self.mark()?;
        let kw = self.advance()?; // `class` | `union`
        let is_union = matches!(kw.kind, TokenKind::Keyword(Keyword::Union));
        let fields = self.parse_class_fields()?;
        // It can't be synthesized inside a generic template if its body references a
        // type parameter — the synthesized definition would carry a `Type::Param` that
        // `mono` never reaches. A concrete anonymous aggregate (no parameters) inside a
        // template, or one used as a generic argument at a non-template site, is fine.
        if self.template_params.is_some() && fields.iter().any(|f| type_mentions_param(&f.ty)) {
            return self.err(
                "anonymous class/union types are not supported inside a generic template; name the type",
            );
        }
        let name = anon_aggregate_name(is_union, &fields);
        if self.anon_types.insert(name.clone()) {
            self.known_types.insert(name.clone());
            let def = self.st(
                StmtKind::Class(ClassDef {
                    is_union,
                    name: name.clone(),
                    base: None,
                    fields,
                    // Synthetic anonymous aggregate: never privacy-gated.
                    is_public: true,
                }),
                dm,
            );
            self.pending_types.push(def);
        }
        Ok(Type::Named(name))
    }

    /// A type with no declarator name: a base type plus any pointer stars. Used for a
    /// tuple element, e.g. `U8 *` in `(I64, U8 *)`.
    fn parse_type_no_name(&mut self) -> PResult<Type> {
        let mut ty = self.parse_base_type()?;
        while self.eat(&TokenKind::Star)? {
            ty = Type::Ptr(Box::new(ty));
        }
        Ok(ty)
    }

    /// Parse a tuple type `(T1, …, Tn)` (n ≥ 2) into a deferred [`Type::Tuple`].
    ///
    /// The [`mono`](crate::mono) pass interns each distinct element list as one
    /// canonical synthetic struct `$Tup…` with positional fields `_0`, `_1`, …, so the
    /// tuple rides on the ordinary struct/`sret`/member machinery. A single
    /// parenthesised type `(T)` is just `T`.
    fn parse_tuple_type(&mut self) -> PResult<Type> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut elems = Vec::new();
        loop {
            elems.push(self.parse_type_no_name()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` to close a tuple type")?;
        if elems.len() == 1 {
            return Ok(elems.pop().unwrap()); // `(T)` is just `T`
        }
        // Defer to a `Type::Tuple`; the `mono` pass interns the canonical `$Tup`
        // struct. Like all type resolution, interning happens in one place after
        // parsing.
        Ok(Type::Tuple(elems))
    }

    /// Look-ahead: does the statement begin with a tuple unpack `name (, name)* :=`?
    /// `_` is an identifier, so it is allowed as a discard slot.
    fn looks_like_unpack(&mut self) -> PResult<bool> {
        let mut i = 0;
        loop {
            if !matches!(self.peek_n(i)?.kind, TokenKind::Ident(_)) {
                return Ok(false);
            }
            i += 1;
            match self.peek_n(i)?.kind {
                TokenKind::Comma => i += 1,
                TokenKind::ColonEq => return Ok(true),
                _ => return Ok(false),
            }
            if i > 512 {
                return Ok(false);
            }
        }
    }

    /// Parse a `:=` short declaration into a deferred [`StmtKind::ShortDecl`].
    ///
    /// With one name it is an inferred-type variable declaration: `n := e;` declares
    /// `n` with `e`'s static type. With two or more it is a tuple unpack: `a, b := e;`
    /// declares each name with the corresponding tuple element type, and `_` discards.
    /// This is the sole destructuring syntax — always explicit (marked by `:=`), with
    /// no parentheses and no written types. The [`mono`](crate::mono) pass types the
    /// RHS and desugars it into one declaration per named slot, plus a hidden tuple
    /// temp for an unpack.
    fn parse_unpack(&mut self, m: Mark) -> PResult<Stmt> {
        let mut names: Vec<Option<String>> = Vec::new();
        loop {
            let n = self.expect_ident()?;
            names.push(if n == "_" { None } else { Some(n) });
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::ColonEq, "`:=`")?;
        let rhs = self.parse_assign()?;
        self.expect(&TokenKind::Semicolon, "`;`")?;
        Ok(self.st(StmtKind::ShortDecl { names, rhs }, m))
    }

    /// Parse `*`… `name` `[dim]`… given a base type, returning the declared name and
    /// its fully built type. A `(` after the leading stars introduces a
    /// function-pointer declarator (`ret (*name)(param-types)`).
    fn parse_declarator(&mut self, base: &Type) -> PResult<(String, Type)> {
        let mut ty = base.clone();
        while self.eat(&TokenKind::Star)? {
            ty = Type::Ptr(Box::new(ty));
        }
        if self.at(&TokenKind::LParen)? {
            return self.parse_funcptr_declarator(ty);
        }
        let name = self.expect_ident()?;
        ty = self.parse_array_suffix(ty)?;
        Ok((name, ty))
    }

    /// Parse the `( * name [dim]… ) ( param-types )` tail of a function-pointer
    /// declarator, given the already-parsed return type. An array suffix on the name
    /// (`(*ops[2])(...)`) makes it an array of function pointers, i.e. a dispatch
    /// table.
    fn parse_funcptr_declarator(&mut self, ret: Type) -> PResult<(String, Type)> {
        self.expect(&TokenKind::LParen, "`(`")?;
        self.expect(&TokenKind::Star, "`*` in a function-pointer declarator")?;
        let name = self.expect_ident()?;
        let mut dims = Vec::new();
        while self.eat(&TokenKind::LBracket)? {
            let dim = if self.at(&TokenKind::RBracket)? {
                None
            } else {
                Some(Box::new(self.parse_expr()?))
            };
            self.expect(&TokenKind::RBracket, "`]`")?;
            dims.push(dim);
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        let params = self.parse_param_types()?;
        let mut ty = Type::FuncPtr {
            ret: Box::new(ret),
            params,
        };
        // Wrap the function-pointer type in any array dimensions; the leftmost `[dim]`
        // is the outermost.
        for dim in dims.into_iter().rev() {
            ty = Type::Array(Box::new(ty), dim);
        }
        Ok((name, ty))
    }

    /// Parse a parenthesised list of parameter *types*, as in a function-pointer
    /// signature. An optional name after each type is allowed and ignored.
    fn parse_param_types(&mut self) -> PResult<Vec<Type>> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen)? {
            loop {
                let mut ty = self.parse_base_type()?;
                while self.eat(&TokenKind::Star)? {
                    ty = Type::Ptr(Box::new(ty));
                }
                if matches!(self.peek()?.kind, TokenKind::Ident(_)) {
                    self.advance()?; // an optional parameter name, ignored
                }
                params.push(ty);
                if !self.eat(&TokenKind::Comma)? {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok(params)
    }

    /// Apply any trailing `[dim]` array suffixes to `ty`.
    fn parse_array_suffix(&mut self, ty: Type) -> PResult<Type> {
        let mut dims = Vec::new();
        while self.eat(&TokenKind::LBracket)? {
            let dim = if self.at(&TokenKind::RBracket)? {
                None
            } else {
                Some(Box::new(self.parse_expr()?))
            };
            self.expect(&TokenKind::RBracket, "`]`")?;
            dims.push(dim);
        }
        // Build so the first (leftmost) dimension is the outermost array.
        let mut out = ty;
        for dim in dims.into_iter().rev() {
            out = Type::Array(Box::new(out), dim);
        }
        Ok(out)
    }

    // ---- statements ----

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        self.enter_recursion()?;
        let r = self.parse_stmt_inner();
        self.depth -= 1;
        r
    }
    fn parse_stmt_inner(&mut self) -> PResult<Stmt> {
        let m = self.mark()?;
        // Capture whether this is a top-level item, then clear it so any nested
        // statement (a block body, a control-flow body) parses as non-top-level.
        let top = std::mem::replace(&mut self.top_level, false);

        // Label: `name:`, but not `name::`, which is a scope operator.
        if matches!(self.peek()?.kind, TokenKind::Ident(_))
            && self.peek_n(1)?.kind == TokenKind::Colon
        {
            let name = self.expect_ident()?;
            self.advance()?; // ':'
            return Ok(self.st(StmtKind::Label(name), m));
        }

        // Tuple unpack: `a, b := e;`. The `:=` declares each name, with the element
        // types inferred from the tuple `e`. This is the only destructuring syntax.
        if self.looks_like_unpack()? {
            return self.parse_unpack(m);
        }

        // A generic function definition `Ret Name<T>(…) { … }`. Detected here rather
        // than via `is_type_start` so it is recognised even when the return type is a
        // bare type parameter (`T`, which isn't a known type name). Captured raw; emits
        // no code.
        if self.looks_like_generic_fn()? {
            return self.capture_generic_fn(m);
        }

        let kind = self.peek_kind()?;
        match kind {
            TokenKind::Semicolon => {
                self.advance()?;
                Ok(self.st(StmtKind::Empty, m))
            }
            TokenKind::LBrace => {
                let stmts = self.parse_block()?;
                Ok(self.st(StmtKind::Block(stmts), m))
            }
            TokenKind::Hash => self.parse_preproc(m),
            TokenKind::Keyword(k) => match k {
                Keyword::If => self.parse_if(m),
                Keyword::While => self.parse_while(m),
                Keyword::Do => self.parse_do_while(m),
                Keyword::For => self.parse_for(m),
                Keyword::Switch => self.parse_switch(m),
                Keyword::Case => self.parse_case(m),
                Keyword::Default => {
                    self.advance()?;
                    self.expect(&TokenKind::Colon, "`:`")?;
                    Ok(self.st(StmtKind::Default, m))
                }
                Keyword::Start => {
                    self.advance()?;
                    self.expect(&TokenKind::Colon, "`:`")?;
                    Ok(self.st(StmtKind::SwitchStart, m))
                }
                Keyword::End => {
                    self.advance()?;
                    self.expect(&TokenKind::Colon, "`:`")?;
                    Ok(self.st(StmtKind::SwitchEnd, m))
                }
                Keyword::Break => {
                    self.advance()?;
                    self.expect(&TokenKind::Semicolon, "`;`")?;
                    Ok(self.st(StmtKind::Break, m))
                }
                Keyword::Continue => {
                    self.advance()?;
                    self.expect(&TokenKind::Semicolon, "`;`")?;
                    Ok(self.st(StmtKind::Continue, m))
                }
                Keyword::Return => self.parse_return(m),
                Keyword::Try => {
                    self.advance()?; // `try`
                    let body = self.parse_block()?;
                    self.expect(&TokenKind::Keyword(Keyword::Catch), "`catch`")?;
                    let handler = self.parse_block()?;
                    Ok(self.st(StmtKind::Try { body, handler }, m))
                }
                Keyword::Throw => {
                    self.advance()?; // `throw`
                    // `throw;` re-raises the current exception; `throw expr;` raises a
                    // new value.
                    let value = if self.at(&TokenKind::Semicolon)? {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    self.expect(&TokenKind::Semicolon, "`;`")?;
                    Ok(self.st(StmtKind::Throw(value), m))
                }
                Keyword::Goto => {
                    self.advance()?;
                    let name = self.expect_ident()?;
                    self.expect(&TokenKind::Semicolon, "`;`")?;
                    Ok(self.st(StmtKind::Goto(name), m))
                }
                Keyword::Class | Keyword::Union => {
                    // `class Name { … }` is a definition; `class { … } v;` is a
                    // declaration whose type is an anonymous aggregate.
                    if matches!(self.peek_n(1)?.kind, TokenKind::LBrace) {
                        self.parse_declaration(m, false, top)
                    } else {
                        self.parse_class(m, false)
                    }
                }
                Keyword::Typedef => self.parse_typedef(m, false),
                // `public` is a top-level visibility modifier on the following
                // class/union, function, global, or typedef.
                Keyword::Public => {
                    self.advance()?; // `public`
                    self.parse_public_item(m, top)
                }
                _ if k.is_type() => self.parse_declaration(m, false, top),
                _ => self.parse_expr_stmt(m),
            },
            _ if self.is_type_start()? => self.parse_declaration(m, false, top),
            _ => self.parse_expr_stmt(m),
        }
    }

    fn parse_expr_stmt(&mut self, m: Mark) -> PResult<Stmt> {
        let e = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "`;`")?;
        Ok(self.st(StmtKind::Expr(e), m))
    }

    fn parse_block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::RBrace)? {
            if self.at(&TokenKind::Eof)? {
                return self.err("unexpected end of input, expected `}`");
            }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(stmts)
    }

    fn parse_if(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // if
        // `if type (T is U) … [else …]` is a compile-time type test — the single-case
        // analogue of `switch type`, resolved by `mono` (which keeps the taken branch
        // and discards the other before sema).
        if self.at(&TokenKind::Keyword(Keyword::Type))? {
            return self.parse_type_if(m);
        }
        self.expect(&TokenKind::LParen, "`(`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)`")?;
        let then = Box::new(self.parse_stmt()?);
        let else_ = if self.at(&TokenKind::Keyword(Keyword::Else))? {
            self.advance()?;
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        Ok(self.st(StmtKind::If { cond, then, else_ }, m))
    }

    /// Parse a compile-time type test `if type (T is U) … [else …]` (`if` already
    /// consumed, `type` next). Both sides are types — typically a type parameter on the
    /// left — related by `is` (or `is not` to negate). It desugars to a one-arm
    /// [`StmtKind::TypeSwitch`] so `mono` selects the branch for the concrete `T` and
    /// discards the other (an ill-typed dead branch never reaches sema), like `switch type`.
    fn parse_type_if(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // `type`
        self.expect(&TokenKind::LParen, "`(` after `if type`")?;
        let lhs = self.parse_type_no_name()?;
        if !self.eat_word("is")? {
            return self.err("`if type` expects `is`, e.g. `if type (T is F64)`");
        }
        let negate = self.eat_word("not")?; // `is not` negates the test
        let rhs = self.parse_type_no_name()?;
        self.expect(&TokenKind::RParen, "`)`")?;
        let then = self.parse_stmt()?;
        let else_ = if self.eat(&TokenKind::Keyword(Keyword::Else))? {
            Some(self.parse_stmt()?)
        } else {
            None
        };
        // `T == rhs ? then : else`; `!=` is the same with the branches swapped. The arm
        // runs when the scrutinee resolves to `rhs`, the default otherwise.
        let (arm, default) = if negate {
            (else_, Some(then))
        } else {
            (Some(then), else_)
        };
        Ok(self.st(
            StmtKind::TypeSwitch {
                on: TypeSwitchOn::Ty(lhs),
                arms: vec![(rhs, arm.map(|s| vec![s]).unwrap_or_default())],
                default: default.map(|s| vec![s]),
            },
            m,
        ))
    }

    fn parse_while(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // while
        self.expect(&TokenKind::LParen, "`(`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)`")?;
        let body = Box::new(self.parse_stmt()?);
        Ok(self.st(StmtKind::While { cond, body }, m))
    }

    fn parse_do_while(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // do
        let body = Box::new(self.parse_stmt()?);
        self.expect(&TokenKind::Keyword(Keyword::While), "`while`")?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::Semicolon, "`;`")?;
        Ok(self.st(StmtKind::DoWhile { body, cond }, m))
    }

    fn parse_for(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // for
        self.expect(&TokenKind::LParen, "`(`")?;

        // Init clause: empty, a declaration, or an expression.
        let init = if self.eat(&TokenKind::Semicolon)? {
            None
        } else {
            let im = self.mark()?;
            let s = if self.is_type_start()? {
                let base = self.parse_base_type()?;
                let dm = self.mark()?;
                let first = self.parse_declarator(&base)?;
                let decls = self.parse_var_decls(&base, first, dm)?;
                self.st(StmtKind::VarDecl { decls }, im)
            } else {
                let e = self.parse_expr()?;
                self.st(StmtKind::Expr(e), im)
            };
            self.expect(&TokenKind::Semicolon, "`;`")?;
            Some(Box::new(s))
        };

        let cond = if self.at(&TokenKind::Semicolon)? {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(&TokenKind::Semicolon, "`;`")?;

        let step = if self.at(&TokenKind::RParen)? {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(&TokenKind::RParen, "`)`")?;

        let body = Box::new(self.parse_stmt()?);
        Ok(self.st(
            StmtKind::For {
                init,
                cond,
                step,
                body,
            },
            m,
        ))
    }

    fn parse_switch(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // switch
        // `switch type (…)` is a compile-time type switch, resolved by `mono`.
        if self.at(&TokenKind::Keyword(Keyword::Type))? {
            return self.parse_type_switch(m);
        }
        // HolyC allows both `switch (x)` and the bracketed `switch [x]`.
        let cond = if self.eat(&TokenKind::LBracket)? {
            let e = self.parse_expr()?;
            self.expect(&TokenKind::RBracket, "`]`")?;
            e
        } else {
            self.expect(&TokenKind::LParen, "`(` or `[`")?;
            let e = self.parse_expr()?;
            self.expect(&TokenKind::RParen, "`)`")?;
            e
        };
        let body = Box::new(self.parse_stmt()?);
        Ok(self.st(StmtKind::Switch { cond, body }, m))
    }

    fn parse_case(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // case
        let lo = self.parse_assign()?;
        let hi = if self.eat(&TokenKind::DotDotDot)? {
            Some(self.parse_assign()?)
        } else {
            None
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        Ok(self.st(StmtKind::Case { lo, hi }, m))
    }

    /// Parse a compile-time type switch (`switch type` already seen). Each `case`
    /// label is a *type*; arm bodies are ordinary statement lists. The
    /// [`mono`](crate::mono) pass selects the arm matching the scrutinee's concrete
    /// type and discards the rest.
    fn parse_type_switch(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // `type`
        self.expect(&TokenKind::LParen, "`(` after `switch type`")?;
        // The scrutinee is a type (incl. a type parameter) when it looks like one,
        // else an expression whose static type `mono` reads.
        let on = if self.is_type_start()? {
            TypeSwitchOn::Ty(self.parse_type_no_name()?)
        } else {
            TypeSwitchOn::Val(Box::new(self.parse_expr()?))
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::LBrace, "`{` to open the type-switch body")?;
        let mut arms: Vec<(Type, Vec<Stmt>)> = Vec::new();
        let mut default: Option<Vec<Stmt>> = None;
        while !self.at(&TokenKind::RBrace)? {
            if self.eat(&TokenKind::Keyword(Keyword::Default))? {
                self.expect(&TokenKind::Colon, "`:`")?;
                default = Some(self.parse_type_switch_arm_body()?);
            } else if self.eat(&TokenKind::Keyword(Keyword::Case))? {
                let ty = self.parse_type_no_name()?;
                self.expect(&TokenKind::Colon, "`:`")?;
                arms.push((ty, self.parse_type_switch_arm_body()?));
            } else {
                return self.err("expected `case`, `default`, or `}` in a type switch");
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(self.st(StmtKind::TypeSwitch { on, arms, default }, m))
    }

    /// Statements of one type-switch arm: everything up to the next `case`,
    /// `default`, or the closing `}`.
    fn parse_type_switch_arm_body(&mut self) -> PResult<Vec<Stmt>> {
        let mut body = Vec::new();
        while !self.at(&TokenKind::RBrace)?
            && !self.at(&TokenKind::Keyword(Keyword::Case))?
            && !self.at(&TokenKind::Keyword(Keyword::Default))?
        {
            body.push(self.parse_stmt()?);
        }
        Ok(body)
    }

    fn parse_return(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // return
        let val = if self.at(&TokenKind::Semicolon)? {
            None
        } else {
            let first = self.parse_assign()?;
            if self.at(&TokenKind::Comma)? {
                // `return a, b, …;` is a multi-value return: a tuple literal of the
                // function's (tuple) return type.
                let mut items = vec![first];
                while self.eat(&TokenKind::Comma)? {
                    items.push(self.parse_assign()?);
                }
                Some(self.ex(ExprKind::InitList(items), m))
            } else {
                Some(first)
            }
        };
        self.expect(&TokenKind::Semicolon, "`;`")?;
        Ok(self.st(StmtKind::Return(val), m))
    }

    fn parse_preproc(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // #
        let directive = self.expect_ident()?;
        match directive.as_str() {
            "include" => {
                let t = self.advance()?;
                match t.kind {
                    TokenKind::Str(path) => Ok(self.st(StmtKind::Include(path), m)),
                    other => Err(ParseError {
                        message: format!("expected a string path after #include, found {other:?}"),
                        pos: t.span.pos,
                    }),
                }
            }
            other => self.err(format!("unsupported preprocessor directive `#{other}`")),
        }
    }

    // ---- declarations ----

    /// Register a type alias from `typedef <type> <name>;`.
    ///
    /// The alias is resolved at parse time, so it never reaches the AST as a `Named`
    /// type. It produces no runtime node, just an `Empty` statement. Aliases must
    /// precede their use.
    fn parse_typedef(&mut self, m: Mark, _is_public: bool) -> PResult<Stmt> {
        // `typedef` aliases are resolved at parse time and are global to the parse
        // stream, so a `public` modifier is accepted but has no effect (a known
        // simplification: type aliases are not file-scoped).
        self.advance()?; // `typedef`
        let base = self.parse_base_type()?;
        // `typedef <ret> (*)(<args>) Name;` — an anonymous function-pointer type with the
        // alias name *after* it, the consistent `typedef <type> <name>` shape. Detected by
        // the `(*)` immediately following the return type (the named form `(*F)(…)` has an
        // identifier where this has `)`).
        if self.at(&TokenKind::LParen)?
            && self.peek_n(1)?.kind == TokenKind::Star
            && self.peek_n(2)?.kind == TokenKind::RParen
        {
            self.advance()?; // `(`
            self.advance()?; // `*`
            self.advance()?; // `)`
            let params = self.parse_param_types()?;
            let ty = Type::FuncPtr {
                ret: Box::new(base),
                params,
            };
            let name = self.expect_ident()?;
            self.expect(&TokenKind::Semicolon, "`;`")?;
            self.known_types.insert(name.clone());
            self.type_aliases.insert(name, ty);
            return Ok(self.st(StmtKind::Empty, m));
        }
        let (name, ty) = self.parse_declarator(&base)?;
        // The C-style named function-pointer `typedef` (`typedef I64 (*Name)(args);`),
        // with the name buried inside the declarator, is rejected: use the trailing-name
        // form above, or the keyword-less `I64 (*Name)(args);`.
        if matches!(ty, Type::FuncPtr { .. }) {
            return self.err(format!(
                "a function-pointer `typedef` puts the name after the type: \
                 `typedef <ret> (*)(<args>) {name};` (or use the keyword-less \
                 `<ret> (*{name})(<args>);`)"
            ));
        }
        self.expect(&TokenKind::Semicolon, "`;`")?;
        self.known_types.insert(name.clone());
        self.type_aliases.insert(name, ty);
        Ok(self.st(StmtKind::Empty, m))
    }

    /// Dispatch the top-level declaration that follows a consumed `public` modifier.
    fn parse_public_item(&mut self, m: Mark, top_level: bool) -> PResult<Stmt> {
        // A generic function definition (`public Ret Name<T>(…)`) is detected the same
        // way as in `parse_stmt`, since its return type may be a bare type parameter
        // that `is_type_start` doesn't recognise. Its monomorphized instances are
        // always public, so the marker needs no further threading.
        if self.looks_like_generic_fn()? {
            return self.capture_generic_fn(m);
        }
        let k = self.peek_kind()?;
        match k {
            TokenKind::Keyword(Keyword::Class) | TokenKind::Keyword(Keyword::Union) => {
                if matches!(self.peek_n(1)?.kind, TokenKind::LBrace) {
                    self.parse_declaration(m, true, top_level)
                } else {
                    self.parse_class(m, true)
                }
            }
            TokenKind::Keyword(Keyword::Typedef) => self.parse_typedef(m, true),
            TokenKind::Keyword(kw) if kw.is_type() => self.parse_declaration(m, true, top_level),
            _ if self.is_type_start()? => self.parse_declaration(m, true, top_level),
            _ => self.err(
                "`public` must precede a function, global, `class`, `union`, or `typedef` declaration",
            ),
        }
    }

    /// A declaration that begins with a type: either a function (definition or
    /// prototype) or a variable declaration list. `is_public` is set when a leading
    /// `public` modifier was consumed.
    fn parse_declaration(&mut self, m: Mark, is_public: bool, top_level: bool) -> PResult<Stmt> {
        let base = self.parse_base_type()?;
        let dm = self.mark()?;
        let (name, ty) = self.parse_declarator(&base)?;

        // A bare *top-level* function-pointer declarator with no initializer names a
        // *type*, i.e. a `typedef` without the keyword:
        //     I64 (*BinOp)(I64, I64);   // defines the type `BinOp`
        // An initializer (`= …`) makes it an ordinary global instead, and the same shape
        // at function-local scope always stays a variable.
        if top_level && matches!(ty, Type::FuncPtr { .. }) && self.at(&TokenKind::Semicolon)? {
            self.advance()?; // `;`
            self.known_types.insert(name.clone());
            self.type_aliases.insert(name, ty);
            return Ok(self.st(StmtKind::Empty, m));
        }

        if self.at(&TokenKind::LParen)? {
            let (params, varargs) = self.parse_params()?;
            let body = if self.at(&TokenKind::LBrace)? {
                Some(self.parse_block()?)
            } else {
                self.expect(&TokenKind::Semicolon, "`;` or a function body")?;
                None
            };
            Ok(self.st(
                StmtKind::Func(FuncDef {
                    ret: ty,
                    name,
                    params,
                    varargs,
                    body,
                    is_public,
                }),
                m,
            ))
        } else {
            let mut decls = self.parse_var_decls(&base, (name, ty), dm)?;
            self.expect(&TokenKind::Semicolon, "`;`")?;
            // A `public` modifier applies to every name in the declaration list.
            if is_public {
                for d in &mut decls {
                    d.is_public = true;
                }
            }
            Ok(self.st(StmtKind::VarDecl { decls }, m))
        }
    }

    /// Finish a variable declaration list whose first declarator is already parsed.
    /// Does not consume the trailing `;`.
    fn parse_var_decls(
        &mut self,
        base: &Type,
        first: (String, Type),
        first_mark: Mark,
    ) -> PResult<Vec<Declarator>> {
        let mut decls = vec![self.finish_declarator(first, first_mark)?];
        while self.eat(&TokenKind::Comma)? {
            let dm = self.mark()?;
            let d = self.parse_declarator(base)?;
            decls.push(self.finish_declarator(d, dm)?);
        }
        Ok(decls)
    }

    fn finish_declarator(&mut self, (name, ty): (String, Type), m: Mark) -> PResult<Declarator> {
        let mut init = if self.eat(&TokenKind::Eq)? {
            Some(self.parse_initializer()?)
        } else {
            None
        };
        // `T a[] = {...}` and `U8 s[] = "..."` infer the outermost array length.
        let ty = infer_array_len(ty, init.as_ref());
        // Once the size is known, `U8 s[N] = "..."` desugars to a byte brace list.
        if let Some(e) = &init {
            if let Some(list) = string_array_init(&ty, e) {
                init = Some(list);
            }
        }
        Ok(Declarator {
            name,
            ty,
            init,
            span: self.finish(m),
            is_public: false,
        })
    }

    /// An initialiser is either a brace-enclosed aggregate list or an ordinary
    /// assignment-level expression.
    fn parse_initializer(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::LBrace)? {
            self.parse_init_list()
        } else {
            self.parse_assign()
        }
    }

    /// Parse `{ init, init, ... }`, allowing nested lists and a trailing comma. When
    /// the list opens with a `.`, it is instead the designated form
    /// `{ .field = init, ... }`.
    fn parse_init_list(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        if self.at(&TokenKind::Dot)? {
            return self.parse_designated_init(m);
        }
        let mut items = Vec::new();
        while !self.at(&TokenKind::RBrace)? {
            items.push(self.parse_initializer()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(self.ex(ExprKind::InitList(items), m))
    }

    /// Parse a designated class initializer `{ .field = init, ... }` (the opening `{`
    /// is already consumed). Each item is a field name, `=`, and an initialiser, which
    /// may itself be a nested brace list.
    fn parse_designated_init(&mut self, m: Mark) -> PResult<Expr> {
        let mut items = Vec::new();
        while !self.at(&TokenKind::RBrace)? {
            self.expect(&TokenKind::Dot, "`.`")?;
            let name = self.expect_ident()?;
            self.expect(&TokenKind::Eq, "`=`")?;
            let value = self.parse_initializer()?;
            items.push((name, value));
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(self.ex(ExprKind::DesignatedInit(items), m))
    }

    fn parse_params(&mut self) -> PResult<(Vec<Param>, bool)> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        let mut varargs = false;
        if !self.at(&TokenKind::RParen)? {
            loop {
                if self.eat(&TokenKind::DotDotDot)? {
                    varargs = true;
                    break;
                }
                let pm = self.mark()?;
                let base = self.parse_base_type()?;
                let mut ty = base;
                while self.eat(&TokenKind::Star)? {
                    ty = Type::Ptr(Box::new(ty));
                }
                // A function-pointer parameter: `ret (*name)(types)`.
                let name = if self.at(&TokenKind::LParen)? {
                    let (n, fpty) = self.parse_funcptr_declarator(ty)?;
                    ty = fpty;
                    Some(n)
                } else {
                    // The parameter name is optional; prototypes may omit it.
                    let name = if matches!(self.peek()?.kind, TokenKind::Ident(_)) {
                        Some(self.expect_ident()?)
                    } else {
                        None
                    };
                    ty = self.parse_array_suffix(ty)?;
                    name
                };
                let default = if self.eat(&TokenKind::Eq)? {
                    Some(self.parse_assign()?)
                } else {
                    None
                };
                params.push(Param {
                    ty,
                    name,
                    default,
                    span: self.finish(pm),
                });
                if !self.eat(&TokenKind::Comma)? {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok((params, varargs))
    }

    fn parse_class(&mut self, m: Mark, is_public: bool) -> PResult<Stmt> {
        let kw = self.advance()?; // class | union
        let is_union = matches!(kw.kind, TokenKind::Keyword(Keyword::Union));
        let name = self.expect_ident()?;
        // A generic template `class Name<T, …> { … }`. Registered for later
        // monomorphization; it emits no class of its own. (Monomorphized instances are
        // always public, so a `public` modifier here is accepted but redundant.)
        if self.at(&TokenKind::Lt)? {
            return self.parse_generic_class(is_union, name, m);
        }
        // Register the type up front so self-referential fields (`Foo *next;`) are
        // recognised while parsing the body.
        self.known_types.insert(name.clone());

        let base = if self.eat(&TokenKind::Colon)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let fields = self.parse_class_fields()?;
        self.eat(&TokenKind::Semicolon)?; // optional trailing `;`

        Ok(self.st(
            StmtKind::Class(ClassDef {
                is_union,
                name,
                base,
                fields,
                is_public,
            }),
            m,
        ))
    }

    /// Parse a `{ field; field; … }` aggregate body into its declarators. An embedded
    /// `union`, anonymous or named, is handled specially.
    fn parse_class_fields(&mut self) -> PResult<Vec<Declarator>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace)? {
            if self.at(&TokenKind::Eof)? {
                return self.err("unexpected end of input in class body, expected `}`");
            }
            // An embedded `union` becomes its own synthetic type.
            if matches!(self.peek_kind()?, TokenKind::Keyword(Keyword::Union)) {
                self.parse_embedded_union(&mut fields)?;
                continue;
            }
            let field_base = self.parse_base_type()?;
            let dm = self.mark()?;
            let first = self.parse_declarator(&field_base)?;
            fields.push(Declarator {
                name: first.0,
                ty: first.1,
                init: None,
                span: self.finish(dm),
                is_public: false,
            });
            while self.eat(&TokenKind::Comma)? {
                let dm = self.mark()?;
                let (n, t) = self.parse_declarator(&field_base)?;
                fields.push(Declarator {
                    name: n,
                    ty: t,
                    init: None,
                    span: self.finish(dm),
                    is_public: false,
                });
            }
            self.expect(&TokenKind::Semicolon, "`;`")?;
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(fields)
    }

    /// Parse a `union` embedded in a class body and append the resulting member.
    ///
    /// Three forms:
    ///   * `union { … };`         — anonymous; its members are *promoted* into the
    ///     enclosing class and accessed directly, e.g. `obj.field`.
    ///   * `union Name { … } m;`  — inline named union type plus a member `m`.
    ///   * `union Name m;`        — a previously-defined union used as a member.
    ///
    /// Inline definitions become a synthetic top-level union type. A promoted
    /// (anonymous) member is given a generated `$anon…` name that later passes
    /// recognise in order to flatten its members.
    fn parse_embedded_union(&mut self, fields: &mut Vec<Declarator>) -> PResult<()> {
        let dm = self.mark()?;
        self.advance()?; // `union`
        let given_name = if matches!(self.peek_kind()?, TokenKind::Ident(_)) {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let type_name = if self.at(&TokenKind::LBrace)? {
            let n = self.anon_counter;
            self.anon_counter += 1;
            let tyname = given_name.unwrap_or_else(|| format!("$Union{n}"));
            self.known_types.insert(tyname.clone());
            let body = self.parse_class_fields()?;
            let def = self.st(
                StmtKind::Class(ClassDef {
                    is_union: true,
                    name: tyname.clone(),
                    base: None,
                    fields: body,
                    // Synthetic embedded-union type: never privacy-gated.
                    is_public: true,
                }),
                dm,
            );
            self.pending_types.push(def);
            tyname
        } else {
            given_name.ok_or_else(|| ParseError {
                message: "expected a union name or `{` after `union`".into(),
                pos: self.cur_pos(),
            })?
        };

        // A member name makes this an ordinary named member. Otherwise the union is
        // anonymous and its members are promoted, via a `$anon…` placeholder field.
        if matches!(self.peek_kind()?, TokenKind::Ident(_)) {
            let member = self.expect_ident()?;
            let ty = self.parse_array_suffix(Type::Named(type_name))?;
            fields.push(Declarator {
                name: member,
                ty,
                init: None,
                span: self.finish(dm),
                is_public: false,
            });
        } else {
            let n = self.anon_counter;
            self.anon_counter += 1;
            fields.push(Declarator {
                name: format!("$anon{n}"),
                ty: Type::Named(type_name),
                init: None,
                span: self.finish(dm),
                is_public: false,
            });
        }
        self.expect(&TokenKind::Semicolon, "`;`")?;
        Ok(())
    }

    // ---- expressions ----

    /// Full expression, including the top-level comma sequence.
    fn parse_expr(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let first = self.parse_assign()?;
        if !self.at(&TokenKind::Comma)? {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma)? {
            items.push(self.parse_assign()?);
        }
        Ok(self.ex(ExprKind::Comma(items), m))
    }

    /// Assignment level (right-associative).
    fn parse_assign(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let lhs = self.parse_ternary()?;
        if let Some(op) = assign_op(&self.peek()?.kind) {
            self.advance()?;
            let value = self.parse_assign()?;
            return Ok(self.ex(
                ExprKind::Assign {
                    op,
                    target: Box::new(lhs),
                    value: Box::new(value),
                },
                m,
            ));
        }
        Ok(lhs)
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let cond = self.parse_binary(0)?;
        if self.eat(&TokenKind::Question)? {
            let then = self.parse_assign()?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let else_ = self.parse_assign()?;
            return Ok(self.ex(
                ExprKind::Ternary {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    else_: Box::new(else_),
                },
                m,
            ));
        }
        Ok(cond)
    }

    /// Parse binary operators via precedence climbing. `min_bp` is the minimum binding
    /// power an operator must have to be consumed at this level.
    fn parse_binary(&mut self, min_bp: u8) -> PResult<Expr> {
        let m = self.mark()?;
        let mut lhs = self.parse_unary()?;
        loop {
            let (op, bp) = match infix_op(&self.peek()?.kind) {
                Some(x) => x,
                None => break,
            };
            if bp < min_bp {
                break;
            }
            self.advance()?;
            // All these operators are left-associative, so the right side binds one
            // level tighter.
            let rhs = self.parse_binary(bp + 1)?;

            // HolyC chained range comparisons: `a < b < c` means `a < b && b < c`. A
            // run of relational operators at the same precedence desugars to a
            // conjunction of the adjacent comparisons. Each interior operand is
            // duplicated, so a side-effecting one runs twice (`a < f() < b` calls `f`
            // twice).
            if is_chain_cmp(op) && self.next_is_chain_cmp(bp)? {
                let mut chain = self.bin(op, lhs, rhs.clone(), m);
                let mut prev = rhs;
                while self.next_is_chain_cmp(bp)? {
                    let (op2, _) = infix_op(&self.peek()?.kind).unwrap();
                    self.advance()?;
                    let next = self.parse_binary(bp + 1)?;
                    let cmp = self.bin(op2, prev, next.clone(), m);
                    chain = self.bin(BinOp::And, chain, cmp, m);
                    prev = next;
                }
                lhs = chain;
            } else {
                lhs = self.bin(op, lhs, rhs, m);
            }
        }
        Ok(lhs)
    }

    /// Build a binary-expression node.
    fn bin(&mut self, op: BinOp, lhs: Expr, rhs: Expr, m: Mark) -> Expr {
        self.ex(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            m,
        )
    }

    /// Whether the next token is a relational operator at precedence `bp`, the signal
    /// to continue a chained comparison.
    fn next_is_chain_cmp(&mut self, bp: u8) -> PResult<bool> {
        Ok(matches!(infix_op(&self.peek()?.kind), Some((op, b)) if b == bp && is_chain_cmp(op)))
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        self.enter_recursion()?;
        let r = self.parse_unary_inner();
        self.depth -= 1;
        r
    }
    fn parse_unary_inner(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let kind = self.peek_kind()?;

        // Prefix operators.
        let prefix = match kind {
            TokenKind::Not => Some(UnOp::Not),
            TokenKind::Tilde => Some(UnOp::BitNot),
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Plus => Some(UnOp::Pos),
            TokenKind::Star => Some(UnOp::Deref),
            TokenKind::Amp => Some(UnOp::AddrOf),
            TokenKind::PlusPlus => Some(UnOp::PreInc),
            TokenKind::MinusMinus => Some(UnOp::PreDec),
            _ => None,
        };
        if let Some(op) = prefix {
            self.advance()?;
            let expr = self.parse_unary()?;
            return Ok(self.ex(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
                m,
            ));
        }

        if kind == TokenKind::Keyword(Keyword::Sizeof) {
            return self.parse_sizeof(m);
        }
        if kind == TokenKind::Keyword(Keyword::Offset) {
            return self.parse_offset(m);
        }

        // Cast: `(` Type `)` unary. Distinguished from a parenthesised expression by
        // peeking whether a type name follows the `(`.
        if kind == TokenKind::LParen {
            let next = self.peek_n(1)?.kind.clone();
            if self.kind_is_type_start(&next) {
                return self.parse_cast(m);
            }
        }

        self.parse_postfix()
    }

    fn parse_sizeof(&mut self, m: Mark) -> PResult<Expr> {
        self.advance()?; // sizeof
        self.expect(&TokenKind::LParen, "`(`")?;
        // `sizeof(Type)` if a type name follows the `(`. Otherwise `sizeof(expr)`,
        // whose size comes from the expression's static type.
        let arg = if self.is_type_start()? {
            let mut ty = self.parse_base_type()?;
            while self.eat(&TokenKind::Star)? {
                ty = Type::Ptr(Box::new(ty));
            }
            SizeofArg::Type(ty)
        } else {
            SizeofArg::Expr(Box::new(self.parse_expr()?))
        };
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok(self.ex(ExprKind::Sizeof(arg), m))
    }

    /// Parse `offset(ClassName.field[.field...])`, HolyC's `offsetof`. The operand is a
    /// class name followed by a dotted member path. It is not a normal expression,
    /// since the class name is a type rather than a value.
    fn parse_offset(&mut self, m: Mark) -> PResult<Expr> {
        self.advance()?; // offset
        self.expect(&TokenKind::LParen, "`(`")?;
        let class = match self.parse_base_type()? {
            Type::Named(name) => name,
            _ => {
                return Err(ParseError {
                    message: "offset() expects a class member, e.g. offset(Class.field)".into(),
                    pos: m.pos,
                });
            }
        };
        let mut path = Vec::new();
        self.expect(&TokenKind::Dot, "`.`")?;
        path.push(self.expect_ident()?);
        while self.eat(&TokenKind::Dot)? {
            path.push(self.expect_ident()?);
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok(self.ex(ExprKind::Offset { class, path }, m))
    }

    fn parse_cast(&mut self, m: Mark) -> PResult<Expr> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let base = self.parse_base_type()?;
        let mut ty = base;
        while self.eat(&TokenKind::Star)? {
            ty = Type::Ptr(Box::new(ty));
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        let expr = self.parse_unary()?;
        Ok(self.ex(
            ExprKind::Cast {
                ty,
                expr: Box::new(expr),
            },
            m,
        ))
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let mut e = self.parse_primary()?;
        // A generic-function call is deferred to a `GenericCall` node, whether written
        // explicitly as `Name<args>(…)` or inferred as `Name(…)`. The `mono` pass
        // infers the type arguments with a real typer and rewrites it to a concrete
        // call, keeping all monomorphization in one type-directed place after parsing.
        if let ExprKind::Ident(n) = &e.kind {
            if self.generics.fns.contains_key(n)
                && (self.at(&TokenKind::Lt)? || self.at(&TokenKind::LParen)?)
            {
                let name = n.clone();
                let type_args = if self.at(&TokenKind::Lt)? {
                    let params = self.generics.fns[n].params.clone();
                    self.parse_generic_args(&params)?
                } else {
                    Vec::new()
                };
                self.expect(&TokenKind::LParen, "`(`")?;
                let args = self.parse_call_args()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                e = self.ex(
                    ExprKind::GenericCall {
                        name,
                        type_args,
                        args,
                    },
                    m,
                );
            }
        }
        loop {
            let kind = self.peek_kind()?;
            match kind {
                TokenKind::LParen => {
                    self.advance()?;
                    let args = self.parse_call_args()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    e = self.ex(
                        ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                        m,
                    );
                }
                TokenKind::LBracket => {
                    self.advance()?;
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "`]`")?;
                    e = self.ex(
                        ExprKind::Index {
                            base: Box::new(e),
                            index: Box::new(index),
                        },
                        m,
                    );
                }
                TokenKind::Dot => {
                    self.advance()?;
                    let field = self.expect_ident()?;
                    e = self.ex(
                        ExprKind::Member {
                            base: Box::new(e),
                            field,
                            arrow: false,
                        },
                        m,
                    );
                }
                TokenKind::Arrow => {
                    self.advance()?;
                    let field = self.expect_ident()?;
                    e = self.ex(
                        ExprKind::Member {
                            base: Box::new(e),
                            field,
                            arrow: true,
                        },
                        m,
                    );
                }
                TokenKind::PlusPlus => {
                    self.advance()?;
                    e = self.ex(
                        ExprKind::Postfix {
                            op: PostOp::Inc,
                            expr: Box::new(e),
                        },
                        m,
                    );
                }
                TokenKind::MinusMinus => {
                    self.advance()?;
                    e = self.ex(
                        ExprKind::Postfix {
                            op: PostOp::Dec,
                            expr: Box::new(e),
                        },
                        m,
                    );
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_call_args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        if self.at(&TokenKind::RParen)? {
            return Ok(args);
        }
        loop {
            args.push(self.parse_assign()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let m = self.mark()?;
        let t = self.advance()?;
        let kind = match t.kind {
            TokenKind::Int(v) => ExprKind::Int(v),
            TokenKind::Float(v) => ExprKind::Float(v),
            TokenKind::Str(s) => ExprKind::Str(s),
            TokenKind::Char(v) => ExprKind::Char(v),
            TokenKind::Ident(s) => ExprKind::Ident(s),
            TokenKind::LParen => {
                // `(a)` is a parenthesised expression. `(a, b, …)` is a tuple literal:
                // a positional aggregate of the canonical tuple struct, lowered through
                // the ordinary brace-init path once its target type is known.
                let first = self.parse_assign()?;
                if self.eat(&TokenKind::Comma)? {
                    let mut items = vec![first];
                    loop {
                        items.push(self.parse_assign()?);
                        if !self.eat(&TokenKind::Comma)? {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)` to close a tuple literal")?;
                    return Ok(self.ex(ExprKind::InitList(items), m));
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                return Ok(first);
            }
            other => {
                return Err(ParseError {
                    message: format!("expected an expression, found {other:?}"),
                    pos: t.span.pos,
                });
            }
        };
        Ok(self.ex(kind, m))
    }
}

/// If `ty` is an unsized array (`T[]`) initialised with a brace list or a string,
/// fill in the outermost length. `I64 a[] = {1,2,3}` becomes `I64 a[3]`, and
/// `U8 s[] = "abc"` becomes `U8 s[4]` (the bytes plus the NUL). Inner dimensions
/// must already be explicit.
fn infer_array_len(ty: Type, init: Option<&Expr>) -> Type {
    if let (Type::Array(elem, None), Some(e)) = (&ty, init) {
        let len = match &e.kind {
            ExprKind::InitList(items) => Some(items.len()),
            ExprKind::Str(s) => Some(s.len() + 1), // bytes + NUL terminator
            _ => None,
        };
        if let Some(n) = len {
            let dim = Expr::new(ExprKind::Int(n as i64), e.span);
            return Type::Array(elem.clone(), Some(Box::new(dim)));
        }
    }
    ty
}

/// Desugar a string initialiser for a *char array* into a byte brace list, so the
/// ordinary brace-init path (interpreter and both backends) handles it. For example
/// `U8 s[6] = "abc"` becomes `U8 s[6] = {'a','b','c',0}`, and the brace-init then
/// zeroes `s[4]` and `s[5]`.
///
/// The NUL is appended, then the list is capped to a constant size, so an
/// exactly-filled array (`U8 s[3] = "abc"`) drops it — matching C. A string
/// initialiser for a *pointer* (`U8 *p = "abc"`) is left alone, as a pointer to the
/// literal. Returns `None` when this isn't a string-into-char-array initialiser.
fn string_array_init(ty: &Type, init: &Expr) -> Option<Expr> {
    let Type::Array(elem, dim) = ty else {
        return None;
    };
    if !matches!(**elem, Type::U8 | Type::I8) {
        return None;
    }
    let ExprKind::Str(s) = &init.kind else {
        return None;
    };
    let mut bytes: Vec<u8> = s.as_bytes().to_vec();
    bytes.push(0); // NUL terminator
    if let Some(d) = dim {
        if let ExprKind::Int(n) = d.kind {
            bytes.truncate(n.max(0) as usize);
        }
    }
    let items = bytes
        .into_iter()
        .map(|b| Expr::new(ExprKind::Int(b as i64), init.span))
        .collect();
    Some(Expr::new(ExprKind::InitList(items), init.span))
}

/// Map an assignment-operator token to its [`AssignOp`].
fn assign_op(k: &TokenKind) -> Option<AssignOp> {
    Some(match k {
        TokenKind::Eq => AssignOp::Assign,
        TokenKind::PlusEq => AssignOp::Add,
        TokenKind::MinusEq => AssignOp::Sub,
        TokenKind::StarEq => AssignOp::Mul,
        TokenKind::SlashEq => AssignOp::Div,
        TokenKind::PercentEq => AssignOp::Mod,
        TokenKind::AmpEq => AssignOp::BitAnd,
        TokenKind::PipeEq => AssignOp::BitOr,
        TokenKind::CaretEq => AssignOp::BitXor,
        TokenKind::ShlEq => AssignOp::Shl,
        TokenKind::ShrEq => AssignOp::Shr,
        _ => return None,
    })
}

/// Whether `op` participates in HolyC chained range comparisons (`a < b < c`). Only
/// the relational operators do. Equality (`==`/`!=`) deliberately does not, so
/// `a == b == c` keeps its standard C meaning `(a == b) == c`.
fn is_chain_cmp(op: BinOp) -> bool {
    matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge)
}

/// Map an infix-operator token to its [`BinOp`] and binding power. Higher binding
/// power binds tighter; left-associative operators recurse at `bp + 1`.
fn infix_op(k: &TokenKind) -> Option<(BinOp, u8)> {
    Some(match k {
        TokenKind::OrOr => (BinOp::Or, 1),
        TokenKind::AndAnd => (BinOp::And, 2),
        TokenKind::Pipe => (BinOp::BitOr, 3),
        TokenKind::Caret => (BinOp::BitXor, 4),
        TokenKind::Amp => (BinOp::BitAnd, 5),
        TokenKind::EqEq => (BinOp::Eq, 6),
        TokenKind::Ne => (BinOp::Ne, 6),
        TokenKind::Lt => (BinOp::Lt, 7),
        TokenKind::Gt => (BinOp::Gt, 7),
        TokenKind::Le => (BinOp::Le, 7),
        TokenKind::Ge => (BinOp::Ge, 7),
        TokenKind::Shl => (BinOp::Shl, 8),
        TokenKind::Shr => (BinOp::Shr, 8),
        TokenKind::Plus => (BinOp::Add, 9),
        TokenKind::Minus => (BinOp::Sub, 9),
        TokenKind::Star => (BinOp::Mul, 10),
        TokenKind::Slash => (BinOp::Div, 10),
        TokenKind::Percent => (BinOp::Mod, 10),
        _ => return None,
    })
}

/// The canonical name of the tuple type with these element types.
///
/// The mangling is deterministic and injective, so two `(I64, F64)`s anywhere name
/// the same synthetic struct. It keeps the `$Tup` prefix that
/// [`is_tuple_name`](crate::ast::is_tuple_name) tests, and the element list
/// self-delimits via [`mangle_type`].
pub(crate) fn tuple_type_name(elems: &[Type]) -> String {
    let mut s = String::from("$Tup");
    for t in elems {
        s.push_str(&mangle_type(t));
    }
    s
}

/// The canonical name of an anonymous `class`/`union` type with these fields.
///
/// The signature is the ordered list of `(field name, field type)` pairs plus the
/// struct/union kind: field names matter (member access is by name) and array
/// dimensions matter ([`mangle_type`] folds them in). The encoding is injective and
/// self-delimiting — each field is `mangle_ident(name)` (length-prefixed) followed by
/// its `mangle_type` — so two identical anonymous aggregates anywhere name the same
/// synthetic type, and a struct never collides with a union of the same fields. The
/// `$Cls`/`$ClsU` prefix keeps it disjoint from user types, tuples (`$Tup`), and
/// embedded unions (`$Union`/`$anon`).
pub(crate) fn anon_aggregate_name(is_union: bool, fields: &[Declarator]) -> String {
    let mut s = String::from(if is_union { "$ClsU" } else { "$Cls" });
    for f in fields {
        s.push_str(&mangle_ident(&f.name));
        s.push_str(&mangle_type(&f.ty));
    }
    s
}

/// Whether a type mentions a generic type parameter (`Type::Param`) anywhere. Used to
/// reject an anonymous aggregate whose fields are parametric inside a template body.
fn type_mentions_param(t: &Type) -> bool {
    match t {
        Type::Param(_) => true,
        Type::Ptr(inner) | Type::Array(inner, _) => type_mentions_param(inner),
        Type::FuncPtr { ret, params } => {
            type_mentions_param(ret) || params.iter().any(type_mentions_param)
        }
        Type::Generic(_, args) => args
            .iter()
            .any(|a| matches!(a, GenericArg::Type(t) if type_mentions_param(t))),
        Type::Tuple(elems) => elems.iter().any(type_mentions_param),
        _ => false,
    }
}

/// The mangled name of a generic instantiation, e.g. `Vec<I8>` → `3VecI8`,
/// `Vec<U8 *>` → `3VecPU8`, `Pair<I64, F64>` → `4PairI64F64`.
///
/// The scheme is injective — an "Itanium-lite" encoding: every identifier component
/// is length-prefixed ([`mangle_ident`]) and every variable-length list is
/// `E`-terminated, so distinct `(template, args)` pairs always map to distinct
/// strings. That injectivity is load-bearing. Without it, a raw `Named` argument
/// could collide with a structural form: `Vec<U8*>` and a `Vec` of a user type named
/// `PU8` would both mangle to `Vec_PU8`. A template name containing `_` could collide
/// with a multi-arg instantiation: `Pair_I64<F64>` vs `Pair<I64, F64>`. Either
/// silently merges two distinct instances, a latent miscompile.
///
/// These strings are HashMap keys and emitted symbol names, not meant to be
/// human-pretty.
pub(crate) fn mangle_generic(name: &str, args: &[GenericArg]) -> String {
    let mut s = mangle_ident(name);
    for a in args {
        s.push_str(&mangle_arg(a));
    }
    s
}

/// Mangle one generic argument. A type arg mangles like its type; a value arg
/// mangles as `C<n>E` (capital `C`, `E`-terminated, disjoint from the letter-led
/// scalar tokens and the digit-led length prefixes), so `FixedArr<I64, 8>` (`…C8E`)
/// and `FixedArr<I64, 9>` (`…C9E`) stay distinct. By the time `mono` mangles, a value
/// arg has been const-evaluated to an `Int` literal.
fn mangle_arg(a: &GenericArg) -> String {
    match a {
        GenericArg::Type(t) => mangle_type(t),
        GenericArg::Value(e) => match &e.kind {
            ExprKind::Int(n) => format!("C{n}E"),
            _ => unreachable!("generic value argument not folded to an Int before mangling"),
        },
    }
}

/// The names of the *type* parameters (not value params) in a generic parameter
/// list. These go into `template_params` so the body recognizes them as types; value
/// params are deliberately excluded (they parse as ordinary `Expr::Ident`s).
fn type_param_names(params: &[GenericParam]) -> Vec<String> {
    params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type(n, _) => Some(n.clone()),
            GenericParam::Value(_) => None,
        })
        .collect()
}

/// A length-prefixed identifier (`Vec` → `3Vec`). The length prefix makes it
/// self-delimiting, so it can't be confused with a following component or collide
/// with another name plus a separator. Being digit-led also keeps it disjoint from
/// the letter-led scalar tokens (`I64`, …) and structural prefixes
/// (`P`/`Arr`/`Fp`/`Tup`) in [`mangle_type`].
fn mangle_ident(name: &str) -> String {
    format!("{}{}", name.len(), name)
}

fn mangle_type(t: &Type) -> String {
    match t {
        Type::U0 => "U0".into(),
        Type::I8 => "I8".into(),
        Type::U8 => "U8".into(),
        Type::I16 => "I16".into(),
        Type::U16 => "U16".into(),
        Type::I32 => "I32".into(),
        Type::U32 => "U32".into(),
        Type::I64 => "I64".into(),
        Type::U64 => "U64".into(),
        Type::F64 => "F64".into(),
        Type::Bool => "Bool".into(),
        Type::Named(n) => mangle_ident(n),
        Type::Ptr(inner) => format!("P{}", mangle_type(inner)),
        Type::Array(inner, dim) => {
            // `Arr` <len> `E` <inner>. The element count (or `u` when the array is
            // unsized or the dimension isn't a constant) is folded in and `E`-terminated
            // so `I64 a[4]` and `I64 a[8]` don't collide — required now that this drives
            // anonymous-aggregate field mangling, not just generic/tuple arguments.
            let len = dim
                .as_ref()
                .and_then(|d| crate::layout::const_eval(d).ok())
                .map_or_else(|| "u".to_string(), |n| n.to_string());
            format!("Arr{len}E{}", mangle_type(inner))
        }
        Type::FuncPtr { ret, params } => {
            // `Fp` <ret> <params…> `E`. The trailing `E` terminates the parameter
            // list, so two FuncPtrs of different arity can't collide (no mangling
            // component starts with `E`).
            let mut s = format!("Fp{}", mangle_type(ret));
            for p in params {
                s.push_str(&mangle_type(p));
            }
            s.push('E');
            s
        }
        // A bare param mangles like a named type. A deferred application reuses the
        // generic mangling (`Vec<I64>` → `3VecI64`).
        Type::Param(name) => mangle_ident(name),
        Type::Generic(name, args) => mangle_generic(name, args),
        Type::Tuple(elems) => {
            let mut s = String::from("Tup");
            for e in elems {
                s.push_str(&mangle_type(e));
            }
            s.push('E');
            s
        }
    }
}

/// Parse source text into a [`Program`].
///
/// This runs the front end twice over the source. Each pass streams, so the token
/// list is never fully buffered:
///
///   1. A pre-pass streams the *preprocessed* tokens and hoists every `class`/`union`
///      name, so forward references to a type parse correctly.
///   2. The real parse runs the preprocessor again, seeded with those names.
///
/// Re-running the deterministic preprocessor is cheap and keeps both passes lazy.
pub fn parse(src: &str) -> PResult<Program> {
    parse_in_dir(src, std::path::Path::new("."))
}

/// Parse `src`, resolving `#include "..."` relative to `dir`, the directory of the
/// source file. The CLI passes the input file's parent directory; [`parse`] defaults
/// it to the current directory. This is the raw front end, with no implicit prelude.
pub fn parse_in_dir(src: &str, dir: &std::path::Path) -> PResult<Program> {
    parse_core(src, dir, &[], true)
}

/// Parse `src`, resolving `#include "..."` relative to `dir` and angle includes
/// (`#include <name>`) against `search`, the standard-library directories tried in
/// order. The CLIs pass the input file's parent as `dir` and the stdlib directories
/// as `search`. This is the full front end.
pub fn parse_with(
    src: &str,
    dir: &std::path::Path,
    search: &[std::path::PathBuf],
) -> PResult<Program> {
    parse_core(src, dir, search, true)
}

/// The implicit prelude: `builtin.hc`, always in scope under [`parse_with`]. It
/// provides the predefined constants plus the `ArgC`/`ArgV`/`VarArg*` primitives.
fn prelude() -> &'static str {
    crate::embedded_stdlib("builtin.hc").expect("builtin.hc is embedded")
}

fn parse_core(
    src: &str,
    dir: &std::path::Path,
    search: &[std::path::PathBuf],
    with_prelude: bool,
) -> PResult<Program> {
    let (known_types, uses_print, uses_f64tostr) =
        hoist_type_names(src, dir, search, with_prelude)?;
    let mut pp =
        Preprocessor::with_base_dir_and_search(Lexer::new(src), dir.to_path_buf(), search.to_vec());
    if with_prelude {
        // The implicit prelude is `builtin.hc`. The printf family (`<stdio.hc>`) is
        // pulled in on demand whenever the program prints: a bare string statement and
        // the `"fmt", …` comma form lower to `Print` calls, and `Print`/`StrPrint`/…
        // are ordinary HolyC functions, so their bodies must be in scope (there is no
        // dead-code elimination). `<stdio.hc>` transitively pulls in the float formatter,
        // so this also covers `%f`/`%e`/`%g`. `F64ToStr` (the round-trip float→string
        // helper) lives in `<stdlib.hc>`, pulled in the same way. The include guards make
        // a user `#include` of either a no-op.
        let mut p = prelude().to_string();
        if uses_print {
            p.push_str("\n#include <stdio.hc>\n");
        }
        if uses_f64tostr {
            p.push_str("\n#include <stdlib.hc>\n");
        }
        pp = pp.with_prelude(&p);
    }
    let mut parser = Parser::with_known_types(pp, known_types);
    let program = parser.parse_program()?;
    // The `mono` pass resolves every deferred generic construct — generic type and
    // call uses, tuple types, `:=` — into concrete AST before sema/layout/codegen.
    crate::mono::expand(program).map_err(|e| ParseError {
        message: e.message,
        pos: e.pos,
    })
}

/// Stream the preprocessed tokens, descending into `#include`d files, and collect
/// every `class`/`union` *name*. This is what lets a type be used before it is
/// defined, and is why the parser is two-pass. Forward references in *value* positions
/// are handled later by the [`mono`](crate::mono) pass's whole-program typer, so no
/// return-type hoisting is needed here.
fn hoist_type_names(
    src: &str,
    dir: &std::path::Path,
    search: &[std::path::PathBuf],
    with_prelude: bool,
) -> PResult<(HashSet<String>, bool, bool)> {
    let mut pp =
        Preprocessor::with_base_dir_and_search(Lexer::new(src), dir.to_path_buf(), search.to_vec());
    if with_prelude {
        pp = pp.with_prelude(prelude());
    }
    let mut names = HashSet::new();
    // Also note whether the program *prints*, so `parse_core` can pull in `<stdio.hc>`
    // (the printf family) on demand. There is no dead-code elimination, so the
    // `Print`/`StrPrint`/… bodies should be present only when used. A print is either
    // a call to a print-family function by name, or a bare string / `"fmt", …` comma
    // statement — a `Str` at a statement boundary followed by `;` (bare) or `,` (comma
    // form). The boundary check keeps a string *expression* (`U8 *p = "x";`,
    // `f("a", b)`) from pulling the machinery in needlessly. `F64ToStr` (the float
    // round-trip formatter) lives in `<stdlib.hc>`, so it is tracked separately.
    let mut uses_print = false;
    let mut uses_f64tostr = false;
    let mut at_boundary = true; // start of input begins a statement
    let mut pending_str = false; // a Str seen at a statement boundary, awaiting `;`/`,`
    loop {
        let t = pp.next_token()?;
        let (mut next_boundary, mut next_pending) = (false, false);
        match &t.kind {
            TokenKind::Eof => break,
            TokenKind::Keyword(Keyword::Class) | TokenKind::Keyword(Keyword::Union) => {
                if let TokenKind::Ident(name) = &pp.next_token()?.kind {
                    names.insert(name.clone());
                }
            }
            TokenKind::Ident(name) => match name.as_str() {
                "Print" | "StrPrint" | "StrNPrint" | "CatPrint" | "MStrPrint" | "SScan"
                | "FGetC" | "GetChar" | "FGetS" | "GetLine" | "ReadLine" | "PutChar" | "Puts" => {
                    uses_print = true
                }
                "F64ToStr" => uses_f64tostr = true,
                _ => {}
            },
            TokenKind::Str(_) => next_pending = at_boundary,
            TokenKind::Semicolon => {
                if pending_str {
                    uses_print = true;
                }
                next_boundary = true;
            }
            TokenKind::Comma => {
                if pending_str {
                    uses_print = true;
                }
            }
            // `{`, `}`, `)`, and `:` begin a following statement (a block,
            // control-flow body, `else`, or label), so a string just after one is a
            // bare-string statement.
            TokenKind::LBrace | TokenKind::RBrace | TokenKind::RParen | TokenKind::Colon => {
                next_boundary = true;
            }
            _ => {}
        }
        at_boundary = next_boundary;
        pending_str = next_pending;
    }
    Ok((names, uses_print, uses_f64tostr))
}

// ---- generic-template handling ----
//
// These methods only recognize and capture generic templates and the angle-bracket
// grammar; they never instantiate anything. A generic *use* — a type `Vec<I64>` or a
// call `Id<I64>(x)` — is left as a deferred AST node (`Type::Generic`, `Type::Tuple`,
// or `ExprKind::GenericCall`). The [`mono`](crate::mono) pass does all
// monomorphization and type-directed inference after the parse.

impl<S: TokenStream> Parser<S> {
    /// Consume a generic-closing `>`. A `>>` (`Shr`) token is split into two `>`: the
    /// first closes this level, the second is pushed back for the enclosing one. This
    /// is how nested `Vec<Vec<I8>>` parses — the classic angle-bracket problem.
    fn expect_generic_gt(&mut self) -> PResult<()> {
        match self.peek_kind()? {
            TokenKind::Gt => {
                self.advance()?;
                Ok(())
            }
            TokenKind::Shr => {
                let t = self.advance()?;
                let mut sp = t.span;
                sp.start += 1; // span of the leftover second `>`
                self.buf.push_front(Token::new(TokenKind::Gt, sp));
                Ok(())
            }
            _ => self.err("expected `>` to close generic type arguments"),
        }
    }

    /// Skip a balanced `<…>` starting at peek index `start` (which must be `<`),
    /// returning the index just past the matching `>` or `>>`. Returns 0 when
    /// unbalanced, i.e. it ran off the end. Used only for non-consuming look-ahead.
    fn skip_angle(&mut self, start: usize) -> PResult<usize> {
        let mut depth = 0i32;
        let mut i = start;
        loop {
            match self.peek_n(i)?.kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => {
                    depth -= 1;
                    if depth <= 0 {
                        return Ok(i + 1);
                    }
                }
                TokenKind::Shr => {
                    depth -= 2;
                    if depth <= 0 {
                        return Ok(i + 1);
                    }
                }
                TokenKind::Eof => return Ok(0),
                _ => {}
            }
            i += 1;
        }
    }

    /// Skip a balanced `(…)` group starting at look-ahead offset `start` (which must
    /// be the `(`), returning the offset just past the matching `)`, or 0 at EOF.
    fn skip_parens(&mut self, start: usize) -> PResult<usize> {
        let mut depth = 0i32;
        let mut i = start;
        loop {
            match self.peek_n(i)?.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth <= 0 {
                        return Ok(i + 1);
                    }
                }
                TokenKind::Eof => return Ok(0),
                _ => {}
            }
            i += 1;
        }
    }

    /// Non-consuming look-ahead: does the statement begin with a generic function
    /// definition `Ret Name<…>(`? A `<` immediately after the function name is the
    /// signal. No existing declaration has that shape, so this never misfires.
    fn looks_like_generic_fn(&mut self) -> PResult<bool> {
        let mut i = 0;
        // Return type: a type keyword; or any identifier (a class name, or one of the
        // function's own type parameters like `T`), with an optional `<…>` when it
        // names a generic class; or a `(…)` tuple type. Then any pointer stars.
        match self.peek_n(i)?.kind.clone() {
            TokenKind::Keyword(k) if Type::from_keyword(k).is_some() => i += 1,
            TokenKind::Ident(s) => {
                i += 1;
                if self.generics.classes.contains_key(&s) && self.peek_n(i)?.kind == TokenKind::Lt {
                    i = self.skip_angle(i)?;
                    if i == 0 {
                        return Ok(false);
                    }
                }
            }
            TokenKind::LParen => {
                // A tuple return type `(T0, …, Tn)`: skip the balanced parens.
                i = self.skip_parens(i)?;
                if i == 0 {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
        while self.peek_n(i)?.kind == TokenKind::Star {
            i += 1;
        }
        // The function name, then `<` (the type-parameter list), then eventually `(`.
        if !matches!(self.peek_n(i)?.kind, TokenKind::Ident(_)) {
            return Ok(false);
        }
        i += 1;
        if self.peek_n(i)?.kind != TokenKind::Lt {
            return Ok(false);
        }
        let after = self.skip_angle(i)?;
        if after == 0 {
            return Ok(false);
        }
        Ok(self.peek_n(after)?.kind == TokenKind::LParen)
    }

    /// Capture a generic function template, parsing it once into a `FuncDef` with its
    /// type parameters left symbolic: `T` → `Type::Param`, nested `Vec<T>` → a deferred
    /// `Type::Generic`, a body generic call → `ExprKind::GenericCall`, a `:=` →
    /// `StmtKind::ShortDecl`. Registers the template and emits nothing. The
    /// [`mono`](crate::mono) pass substitutes the parameters at each instantiation.
    fn capture_generic_fn(&mut self, m: Mark) -> PResult<Stmt> {
        let mut toks = Vec::new();
        let mut brace = 0i32;
        let mut started = false;
        loop {
            let t = self.advance()?;
            match t.kind {
                TokenKind::LBrace => {
                    brace += 1;
                    started = true;
                }
                TokenKind::RBrace => brace -= 1,
                TokenKind::Eof => {
                    return self.err("unterminated generic function (missing `}`)");
                }
                _ => {}
            }
            let done = started && brace == 0 && matches!(t.kind, TokenKind::RBrace);
            toks.push(t);
            if done {
                break;
            }
        }
        // The value-parameter list's `(` is the first one preceded by the `>` that
        // closes the type-parameter list. A `(…)` tuple *return* type would not match,
        // since its `(` comes first and has no preceding `>`. The matching `<` follows
        // the function name.
        let lp = match toks.iter().enumerate().position(|(idx, t)| {
            t.kind == TokenKind::LParen && idx > 0 && toks[idx - 1].kind == TokenKind::Gt
        }) {
            Some(p) => p,
            None => return self.err_at(m.pos, "generic function: missing `(`"),
        };
        let gt_index = lp - 1;
        // Walk back to the matching `<`.
        let mut depth = 0i32;
        let mut k = gt_index;
        loop {
            match toks[k].kind {
                TokenKind::Gt => depth += 1,
                TokenKind::Lt => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if k == 0 {
                return self.err_at(m.pos, "generic function: malformed type-parameter list");
            }
            k -= 1;
        }
        let lt_index = k;
        if lt_index == 0 {
            return self.err_at(m.pos, "generic function: missing name");
        }
        let name_index = lt_index - 1;
        let name = match &toks[name_index].kind {
            TokenKind::Ident(s) => s.clone(),
            _ => return self.err_at(m.pos, "generic function: name must be an identifier"),
        };
        // Parse the parameter list from the captured tokens, comma-separated. Each
        // group is `type T` / `comparable T` / `int N` (a kind keyword + an ident); a
        // bare `T` (no keyword) is rejected below.
        let mut params: Vec<GenericParam> = Vec::new();
        for group in toks[lt_index + 1..gt_index].split(|t| t.kind == TokenKind::Comma) {
            let (kw, name_tok) = match group {
                [k, n] => (Some(&k.kind), &n.kind),
                [n] => (None, &n.kind),
                [] => continue,
                _ => {
                    return self.err_at(m.pos, "generic function: malformed type-parameter list");
                }
            };
            let TokenKind::Ident(pname) = name_tok else {
                return self.err_at(
                    m.pos,
                    "generic function: parameter name must be an identifier",
                );
            };
            let pname = pname.clone();
            params.push(match kw {
                Some(TokenKind::Keyword(Keyword::Type)) => GenericParam::Type(pname, None),
                Some(TokenKind::Keyword(Keyword::Comparable)) => {
                    GenericParam::Type(pname, Some(Constraint::Comparable))
                }
                Some(TokenKind::Keyword(Keyword::Int)) => GenericParam::Value(pname),
                _ => {
                    return self.err_at(
                        m.pos,
                        "generic parameter must be declared with `type`, `comparable`, or `int` \
                         (e.g. `T Id<type T>(T x)`)",
                    );
                }
            });
        }
        // Register a placeholder first, so a self-recursive generic call in the body is
        // recognized as generic (and thus deferred) while parsing the template. The
        // `mono` pass infers type arguments from the parsed parameter types directly.
        self.generics.fns.insert(
            name.clone(),
            GenericFn {
                params: params.clone(),
                def: FuncDef {
                    ret: Type::U0,
                    name: name.clone(),
                    params: Vec::new(),
                    varargs: false,
                    body: None,
                    is_public: false,
                },
            },
        );
        // Parse the template once into a `FuncDef` with its parameters symbolic: drop
        // the `<T,…>` list, then parse under template scope.
        let mut dropped: Vec<Token> = toks
            .iter()
            .enumerate()
            .filter(|(i, _)| !(*i > name_index && *i <= gt_index))
            .map(|(_, t)| t.clone())
            .collect();
        dropped.push(Token::new(TokenKind::Eof, Span::dummy()));
        let saved_buf = std::mem::take(&mut self.buf);
        let saved_tp = self.template_params.replace(type_param_names(&params));
        self.buf = dropped.into();
        let dm = self.mark()?;
        let parsed = self.parse_declaration(dm, false, false);
        self.template_params = saved_tp;
        self.buf = saved_buf;
        let def = match parsed?.kind {
            StmtKind::Func(f) => f,
            _ => {
                return self.err_at(
                    m.pos,
                    "generic function template is not a function definition",
                );
            }
        };
        if let Some(t) = self.generics.fns.get_mut(&name) {
            t.def = def;
        }
        Ok(self.st(StmtKind::Empty, m))
    }

    /// Parse one generic parameter: `type T` / `comparable T` / `int N`, or a bare
    /// `T` (an unconstrained type param, the backward-compatible form).
    fn parse_generic_param(&mut self) -> PResult<GenericParam> {
        match self.peek_kind()? {
            TokenKind::Keyword(Keyword::Type) => {
                self.advance()?;
                Ok(GenericParam::Type(self.expect_ident()?, None))
            }
            TokenKind::Keyword(Keyword::Comparable) => {
                self.advance()?;
                Ok(GenericParam::Type(
                    self.expect_ident()?,
                    Some(Constraint::Comparable),
                ))
            }
            TokenKind::Keyword(Keyword::Int) => {
                self.advance()?;
                Ok(GenericParam::Value(self.expect_ident()?))
            }
            _ => self.err(
                "generic parameter must be declared with `type`, `comparable`, or `int` \
                 (e.g. `class Vec<type T>`)",
            ),
        }
    }

    /// Parse a generic template `class Name<T, …> { … }` (the leading `<` has already
    /// been peeked). Records the template for the [`mono`](crate::mono) pass and emits
    /// no item.
    fn parse_generic_class(&mut self, is_union: bool, name: String, m: Mark) -> PResult<Stmt> {
        self.advance()?; // `<`
        let mut params = Vec::new();
        loop {
            params.push(self.parse_generic_param()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::Gt, "`>` to close the type-parameter list")?;
        let base = if self.eat(&TokenKind::Colon)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        // Register the template (name + params) before parsing the body, so a
        // self-referential field recognizes the name as generic and defers it — e.g.
        // `HmapEntry<K,V> *next` inside `class HmapEntry<K,V>`.
        self.generics.classes.insert(
            name.clone(),
            GenericClass {
                is_union,
                params: params.clone(),
                base: base.clone(),
                fields: Vec::new(),
            },
        );
        // Parse the body with the *type* parameters in scope, so field types keep them
        // symbolic (`Type::Param` or deferred `Type::Generic`) for `mono` to
        // substitute. Value (`int`) params are left out — they appear as ordinary
        // `Expr::Ident`s (array dims, expressions), never as types.
        let saved = self.template_params.replace(type_param_names(&params));
        let fields = self.parse_class_fields();
        self.template_params = saved;
        let fields = fields?;
        self.eat(&TokenKind::Semicolon)?;
        if let Some(tmpl) = self.generics.classes.get_mut(&name) {
            tmpl.fields = fields;
        }
        Ok(self.st(StmtKind::Empty, m)) // the template itself produces no code
    }

    /// Parse a `<T1, …>` type-argument list, for a generic type use or call. The
    /// arguments stay as parsed `Type`s, possibly themselves deferred `Type::Generic`;
    /// `mono` resolves them.
    /// Parse a `<...>` generic argument list, deciding each position from the
    /// template's declared parameter kinds: an `int` (value) param parses a constant
    /// expression, any other position a type. Out-of-range positions default to a type
    /// so an arity error surfaces later in `mono`.
    fn parse_generic_args(&mut self, params: &[GenericParam]) -> PResult<Vec<GenericArg>> {
        self.expect(&TokenKind::Lt, "`<` for type arguments")?;
        let mut args = Vec::new();
        loop {
            let arg = if matches!(params.get(args.len()), Some(GenericParam::Value(_))) {
                GenericArg::Value(Box::new(self.parse_const_arg()?))
            } else {
                GenericArg::Type(self.parse_type_no_name()?)
            };
            args.push(arg);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect_generic_gt()?;
        Ok(args)
    }

    /// Parse a value (non-type) generic argument: a constant expression. The minimum
    /// binding power (9) admits `+ - * / %`, unary ops, and primaries/`sizeof`/idents,
    /// but stops before shift (8) / relational (7) / bitwise, so a top-level `>`/`>>`
    /// stays a list delimiter (`expect_generic_gt` splits `>>`). Parenthesize anything
    /// looser: `FixedArr<I64, (1 << 10)>`.
    fn parse_const_arg(&mut self) -> PResult<Expr> {
        self.parse_binary(9)
    }
}
