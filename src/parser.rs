//! Recursive-descent parser for HolyC.
//!
//! The parser is generic over a `TokenStream` and pulls tokens lazily through it
//! — in practice a `Preprocessor` wrapping a [`Lexer`], so macros and `#include`s
//! splice in transparently. It keeps only a tiny look-ahead buffer (a couple of
//! tokens), so the complete token stream is never held in memory at once. Parsing
//! is two-pass: a first sweep hoists `class`/`union` names (so a type can be used
//! before it is defined), then the real parse runs.
//!
//! Every node it produces carries a [`Span`]. A node's span runs from the start
//! of its first token to the end of its last token; the parser tracks the end
//! of the most recently consumed token in `prev_end`.

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

/// The start of a node: captured before parsing it, then paired with the end of
/// the last consumed token to form a [`Span`].
#[derive(Clone, Copy)]
struct Mark {
    start: usize,
    pos: Pos,
}

pub struct Parser<S: TokenStream> {
    stream: S,
    /// Look-ahead buffer. Tokens are pulled from the stream only as the parser
    /// peeks past what it has already seen.
    buf: VecDeque<Token>,
    /// Byte offset just past the most recently consumed token, used as the end
    /// of node spans.
    prev_end: usize,
    /// Names that are known to be types — built-in via the lexer, plus
    /// `class`/`union` names hoisted ahead of time and any seen while parsing.
    /// Lets the parser tell `Foo x;` (a declaration) from `Foo * x` (a
    /// multiplication) regardless of definition order.
    known_types: HashSet<String>,
    /// `typedef` aliases mapping a name to the type it stands for. Resolved at
    /// parse time, so an alias never reaches the AST as a `Named` type. Aliases
    /// must be defined before use (the C rule).
    type_aliases: HashMap<String, Type>,
    /// Synthetic type definitions produced while parsing (e.g. an inline/anonymous
    /// `union` embedded in a class). They are injected as top-level items before
    /// the item that referenced them.
    pending_types: Vec<Stmt>,
    /// Counter for naming anonymous embedded unions (`$anonN`).
    anon_counter: u32,
    /// Canonical names of tuple types `(T1, …, Tn)` already injected as synthetic
    /// structs, so each distinct element-list mints exactly one `$Tup…` class.
    tuple_types: HashSet<String>,
    /// Current recursion depth through `parse_unary`/`parse_stmt` (the funnels every
    /// nested expression/statement passes through), so pathologically deep input
    /// fails with a `ParseError` instead of overflowing the stack and aborting.
    depth: u32,
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
            tuple_types: HashSet::new(),
            depth: 0,
        }
    }

    /// Enter a recursive parse frame, erroring if nesting is too deep. Paired with a
    /// `self.depth -= 1` on the success path (an error aborts the whole parse, so a
    /// leftover count is harmless).
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
            let stmt = self.parse_stmt()?;
            // Synthetic types (embedded unions) defined while parsing this item
            // are emitted first, so they're laid out/registered before their use.
            items.append(&mut self.pending_types);
            items.push(stmt);
        }
        Ok(Program { items })
    }

    // ---- token buffer / look-ahead ----

    /// Make sure the buffer holds at least `n + 1` tokens (or has reached Eof).
    fn ensure(&mut self, n: usize) -> PResult<()> {
        while self.buf.len() <= n {
            // Stop pulling once Eof is buffered; the lexer would just keep
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

    /// Clone of the current token kind — handy when matching while needing to
    /// keep calling `&mut self` methods in the arms.
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
        })
    }

    /// Build the span running from `m` to the end of the last consumed token.
    fn finish(&self, m: Mark) -> Span {
        Span::new(m.start, self.prev_end, m.pos)
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
            TokenKind::Keyword(kw) => kw.is_type(),
            TokenKind::Ident(s) => self.known_types.contains(s),
            _ => false,
        }
    }

    fn is_type_start(&mut self) -> PResult<bool> {
        let k = self.peek_kind()?;
        if !matches!(k, TokenKind::LParen) {
            return Ok(self.kind_is_type_start(&k));
        }
        // A `(` begins a tuple type `(T, …)` (a declaration) only if a type starts
        // right after it AND there's a top-level `,` before the matching `)` — which
        // distinguishes it from `(expr)` and a cast `(T)expr`.
        let k1 = self.peek_n(1)?.kind.clone();
        if !self.kind_is_type_start(&k1) {
            return Ok(false);
        }
        let mut depth = 0i32;
        let mut i = 1;
        loop {
            match self.peek_n(i)?.kind.clone() {
                TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RParen | TokenKind::RBracket => {
                    if depth == 0 {
                        return Ok(false); // closed with no top-level comma → not a tuple
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
        let t = self.advance()?;
        match t.kind {
            TokenKind::Keyword(kw) => Type::from_keyword(kw).ok_or_else(|| ParseError {
                message: format!("`{}` is not a type", kw.as_str()),
                pos: t.span.pos,
            }),
            // A `typedef` alias resolves to its target type; any other identifier
            // is a class/union name.
            TokenKind::Ident(s) => Ok(self.type_aliases.get(&s).cloned().unwrap_or(Type::Named(s))),
            other => Err(ParseError {
                message: format!("expected a type, found {other:?}"),
                pos: t.span.pos,
            }),
        }
    }

    /// A type with no declarator name (a tuple element, e.g. `U8 *` in
    /// `(I64, U8 *)`): a base type plus any pointer stars.
    fn parse_type_no_name(&mut self) -> PResult<Type> {
        let mut ty = self.parse_base_type()?;
        while self.eat(&TokenKind::Star)? {
            ty = Type::Ptr(Box::new(ty));
        }
        Ok(ty)
    }

    /// Parse a tuple type `(T1, …, Tn)` (n ≥ 2). Each distinct element-list mints one
    /// canonical synthetic struct `$Tup$…` with positional fields `_0`, `_1`, …, so
    /// the tuple rides on the ordinary struct/`sret`/member machinery. `(T)` (a single
    /// parenthesised type) is just `T`.
    fn parse_tuple_type(&mut self) -> PResult<Type> {
        let m = self.mark()?;
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
        Ok(Type::Named(self.intern_tuple_type(&elems, m)))
    }

    /// Intern the canonical tuple struct for `elems` and return its type name,
    /// injecting the synthetic `class $Tup$…` once (shared with tuple types and
    /// destructuring temps).
    fn intern_tuple_type(&mut self, elems: &[Type], m: Mark) -> String {
        let name = tuple_type_name(elems);
        if self.tuple_types.insert(name.clone()) {
            let fields = elems
                .iter()
                .enumerate()
                .map(|(i, t)| Declarator {
                    name: format!("_{i}"),
                    ty: t.clone(),
                    init: None,
                    span: Span::dummy(),
                })
                .collect();
            let def = self.st(
                StmtKind::Class(ClassDef {
                    is_union: false,
                    name: name.clone(),
                    base: None,
                    fields,
                }),
                m,
            );
            self.known_types.insert(name.clone());
            self.pending_types.push(def);
        }
        name
    }

    /// Does the statement begin with a tuple destructuring pattern — `(` … `)` `=`
    /// with a top-level comma inside? (The comma rules it out as `(lvalue) = e`.)
    fn looks_like_destructure(&mut self) -> PResult<bool> {
        if !matches!(self.peek_kind()?, TokenKind::LParen) {
            return Ok(false);
        }
        let mut depth = 0i32;
        let mut i = 0;
        let mut saw_comma = false;
        loop {
            match self.peek_n(i)?.kind.clone() {
                TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RParen | TokenKind::RBracket => {
                    depth -= 1;
                    if depth == 0 {
                        // The matching `)` of the leading group: a destructure iff
                        // it had a top-level comma and `=` follows.
                        let next = self.peek_n(i + 1)?.kind.clone();
                        return Ok(saw_comma && next == TokenKind::Eq);
                    }
                }
                TokenKind::Comma if depth == 1 => saw_comma = true,
                TokenKind::Eof => return Ok(false),
                _ => {}
            }
            i += 1;
            if i > 512 {
                return Ok(false);
            }
        }
    }

    /// Parse and desugar `(slot, …) = rhs;`. Each slot is a typed binding
    /// (`T name` / `T _`), or — in the all-untyped *assignment* form — an existing
    /// variable (`name` / `_`). The typed form lowers to a hidden tuple temp plus a
    /// declaration per named slot; the untyped form requires a side-effect-free
    /// source and assigns each slot directly. Both ride the existing tuple-struct
    /// member machinery, so no backend changes are needed.
    fn parse_destructure(&mut self, m: Mark) -> PResult<Stmt> {
        self.expect(&TokenKind::LParen, "`(`")?;
        // (declared type or None, binding name or None for `_`)
        let mut slots: Vec<(Option<Type>, Option<String>)> = Vec::new();
        loop {
            let ty = if self.is_type_start()? {
                Some(self.parse_type_no_name()?)
            } else {
                None
            };
            let name = self.expect_ident()?;
            slots.push((ty, if name == "_" { None } else { Some(name) }));
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` to close a destructuring pattern")?;
        self.expect(&TokenKind::Eq, "`=`")?;
        let rhs = self.parse_assign()?;
        self.expect(&TokenKind::Semicolon, "`;`")?;

        let typed = slots.iter().any(|(t, _)| t.is_some());
        if typed && slots.iter().any(|(t, _)| t.is_none()) {
            return self.err_at(
                m.pos,
                "destructuring slots must be either all typed (a declaration) or all untyped (an assignment); use `T _` to discard a typed slot",
            );
        }

        if typed {
            // Declaration form: bind the source to a hidden tuple temp, then declare
            // each named slot from its field — all in one `VarDecl` so the new names
            // land in the enclosing scope (a `Block` would hide them) and later
            // declarators can read the temp. `T _` slots keep their place in the
            // tuple shape but bind nothing.
            let elems: Vec<Type> = slots.iter().map(|(t, _)| t.clone().unwrap()).collect();
            let tup = self.intern_tuple_type(&elems, m);
            let tmp = format!("$dst{}", m.start);
            let mut decls = vec![Declarator {
                name: tmp.clone(),
                ty: Type::Named(tup),
                init: Some(rhs),
                span: self.finish(m),
            }];
            for (i, (ty, name)) in slots.iter().enumerate() {
                let Some(name) = name else { continue };
                decls.push(Declarator {
                    name: name.clone(),
                    ty: ty.clone().unwrap(),
                    init: Some(self.tuple_field(&tmp, i, m)),
                    span: self.finish(m),
                });
            }
            return Ok(self.st(StmtKind::VarDecl { decls }, m));
        }

        let mut stmts: Vec<Stmt> = Vec::new();
        {
            // Assignment form: assign each slot from the source's fields. The source
            // is read once per slot, so it must be side-effect free.
            if !is_simple_source(&rhs) {
                return self.err_at(
                    m.pos,
                    "an untyped destructuring assignment needs a simple source (a variable or field path); bind with types to capture a computed tuple",
                );
            }
            for (i, (_, name)) in slots.iter().enumerate() {
                let Some(name) = name else { continue };
                let target = self.ex(ExprKind::Ident(name.clone()), m);
                let field = self.tuple_member(rhs.clone(), i, m);
                let assign = self.ex(
                    ExprKind::Assign {
                        op: AssignOp::Assign,
                        target: Box::new(target),
                        value: Box::new(field),
                    },
                    m,
                );
                stmts.push(self.st(StmtKind::Expr(assign), m));
            }
        }
        Ok(self.st(StmtKind::Block(stmts), m))
    }

    /// `<var>._<i>` — read field `i` of a tuple-typed variable.
    fn tuple_field(&self, var: &str, i: usize, m: Mark) -> Expr {
        let base = self.ex(ExprKind::Ident(var.to_string()), m);
        self.tuple_member(base, i, m)
    }

    /// `<base>._<i>` — positional tuple field access.
    fn tuple_member(&self, base: Expr, i: usize, m: Mark) -> Expr {
        self.ex(
            ExprKind::Member {
                base: Box::new(base),
                field: format!("_{i}"),
                arrow: false,
            },
            m,
        )
    }

    /// Parse `*`… `name` `[dim]`… given a base type, returning the declared name
    /// and its fully built type. A `(` after the leading stars introduces a
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
    /// declarator, given the already-parsed return type. An array suffix on the
    /// name (`(*ops[2])(...)`) makes it an array of function pointers (a dispatch
    /// table).
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
        // Wrap the function-pointer type in any array dimensions (outermost is
        // the leftmost `[dim]`).
        for dim in dims.into_iter().rev() {
            ty = Type::Array(Box::new(ty), dim);
        }
        Ok((name, ty))
    }

    /// Parse a parenthesised list of parameter *types* (an optional name after
    /// each is allowed and ignored), as in a function-pointer signature.
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

        // Label: `name:` (but not `name::`, which is a scope operator).
        if matches!(self.peek()?.kind, TokenKind::Ident(_))
            && self.peek_n(1)?.kind == TokenKind::Colon
        {
            let name = self.expect_ident()?;
            self.advance()?; // ':'
            return Ok(self.st(StmtKind::Label(name), m));
        }

        // Tuple destructuring: `(T0 a, T1 b) = e;` or `(a, b) = e;`.
        if self.looks_like_destructure()? {
            return self.parse_destructure(m);
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
                Keyword::Goto => {
                    self.advance()?;
                    let name = self.expect_ident()?;
                    self.expect(&TokenKind::Semicolon, "`;`")?;
                    Ok(self.st(StmtKind::Goto(name), m))
                }
                Keyword::Class | Keyword::Union => self.parse_class(m),
                Keyword::Typedef => self.parse_typedef(m),
                _ if k.is_type() => self.parse_declaration(m),
                _ => self.parse_expr_stmt(m),
            },
            _ if self.is_type_start()? => self.parse_declaration(m),
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
        // HolyC allows both `switch (x)` and the bounded `switch [x]`.
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

    fn parse_return(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // return
        let val = if self.at(&TokenKind::Semicolon)? {
            None
        } else {
            let first = self.parse_assign()?;
            if self.at(&TokenKind::Comma)? {
                // `return a, b, …;` — a multi-value return, i.e. a tuple literal of
                // the function's (tuple) return type.
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

    /// A declaration that begins with a type: either a function (def or
    /// prototype) or a variable declaration list.
    /// `typedef <type> <name>;` — register a type alias. The alias is resolved at
    /// parse time (so it never reaches the AST as a `Named` type) and produces no
    /// runtime node (an `Empty` statement). Aliases must precede their use.
    fn parse_typedef(&mut self, m: Mark) -> PResult<Stmt> {
        self.advance()?; // `typedef`
        let base = self.parse_base_type()?;
        let (name, ty) = self.parse_declarator(&base)?;
        self.expect(&TokenKind::Semicolon, "`;`")?;
        self.known_types.insert(name.clone());
        self.type_aliases.insert(name, ty);
        Ok(self.st(StmtKind::Empty, m))
    }

    fn parse_declaration(&mut self, m: Mark) -> PResult<Stmt> {
        let base = self.parse_base_type()?;
        let dm = self.mark()?;
        let (name, ty) = self.parse_declarator(&base)?;

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
                }),
                m,
            ))
        } else {
            let decls = self.parse_var_decls(&base, (name, ty), dm)?;
            self.expect(&TokenKind::Semicolon, "`;`")?;
            Ok(self.st(StmtKind::VarDecl { decls }, m))
        }
    }

    /// Finish a variable declaration list whose first declarator is already
    /// parsed. Does not consume the trailing `;`.
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
        // `T a[] = {...}` / `U8 s[] = "..."` infers the outermost array length.
        let ty = infer_array_len(ty, init.as_ref());
        // `U8 s[N] = "..."` desugars to a byte brace list (after the size is known).
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
        })
    }

    /// An initialiser is either a brace-enclosed aggregate list or an ordinary
    /// (assignment-level) expression.
    fn parse_initializer(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::LBrace)? {
            self.parse_init_list()
        } else {
            self.parse_assign()
        }
    }

    /// `{ init, init, ... }` (nested lists and a trailing comma are allowed), or
    /// a designated form `{ .field = init, ... }` when the list opens with a `.`.
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

    /// `{ .field = init, ... }` — a designated class initializer (the opening
    /// `{` is already consumed). Each item is a field name, `=`, and an
    /// initialiser (itself possibly a nested brace list).
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
                    // The parameter name is optional (prototypes may omit it).
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

    fn parse_class(&mut self, m: Mark) -> PResult<Stmt> {
        let kw = self.advance()?; // class | union
        let is_union = matches!(kw.kind, TokenKind::Keyword(Keyword::Union));
        let name = self.expect_ident()?;
        // Register the type up front so self-referential fields (`Foo *next;`)
        // are recognised while parsing the body.
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
            }),
            m,
        ))
    }

    /// Parse a `{ field; field; … }` aggregate body into its declarators. An
    /// embedded `union` (anonymous or named) is handled specially.
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
            });
            while self.eat(&TokenKind::Comma)? {
                let dm = self.mark()?;
                let (n, t) = self.parse_declarator(&field_base)?;
                fields.push(Declarator {
                    name: n,
                    ty: t,
                    init: None,
                    span: self.finish(dm),
                });
            }
            self.expect(&TokenKind::Semicolon, "`;`")?;
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(fields)
    }

    /// Parse a `union` embedded in a class body and append the resulting member.
    /// Forms:
    ///   * `union { … };`         — anonymous; its members are *promoted* into the
    ///     enclosing class (accessed directly, e.g. `obj.field`).
    ///   * `union Name { … } m;`  — inline named union type plus a member `m`.
    ///   * `union Name m;`        — a previously-defined union used as a member.
    ///
    /// Inline definitions become a synthetic top-level union type. A promoted
    /// (anonymous) member is given a generated `$anon…` name the later passes
    /// recognise to flatten its members.
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

        // A member name → an ordinary named member; otherwise the union is
        // anonymous and its members are promoted (a `$anon…` placeholder field).
        if matches!(self.peek_kind()?, TokenKind::Ident(_)) {
            let member = self.expect_ident()?;
            let ty = self.parse_array_suffix(Type::Named(type_name))?;
            fields.push(Declarator {
                name: member,
                ty,
                init: None,
                span: self.finish(dm),
            });
        } else {
            let n = self.anon_counter;
            self.anon_counter += 1;
            fields.push(Declarator {
                name: format!("$anon{n}"),
                ty: Type::Named(type_name),
                init: None,
                span: self.finish(dm),
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

    /// Binary operators via precedence climbing. `min_bp` is the minimum binding
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
            // All these operators are left-associative, so the right side binds
            // one level tighter.
            let rhs = self.parse_binary(bp + 1)?;

            // HolyC chained range comparisons: `a < b < c` means `a < b && b < c`.
            // A run of relational operators at the same precedence desugars to a
            // conjunction of the adjacent comparisons. Each interior operand is
            // duplicated, so keep it side-effect-free (`a < f() < b` calls twice).
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

    /// Whether the next token is a relational operator at precedence `bp` — the
    /// signal to continue a chained comparison.
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

        // Cast: `(` Type `)` unary. Distinguished from a parenthesised
        // expression by peeking whether a type name follows the `(`.
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
        // `sizeof(Type)` if a type name follows the `(`, otherwise
        // `sizeof(expr)` — its size comes from the expression's static type.
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

    /// `offset(ClassName.field[.field...])` — HolyC's `offsetof`. The operand is
    /// a class name followed by a dotted member path (not a normal expression,
    /// since the class name is a type rather than a value).
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
                // `(a)` is a parenthesised expression; `(a, b, …)` is a tuple literal
                // (a positional aggregate of the canonical tuple struct, so it lowers
                // through the ordinary brace-init path once its target type is known).
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
/// fill in the outermost length: `I64 a[] = {1,2,3}` becomes `I64 a[3]`, and
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

/// Desugar a string initialiser for a **char array** into a byte brace list, so
/// the ordinary brace-init path (interpreter + both backends) handles it:
/// `U8 s[6] = "abc"` becomes `U8 s[6] = {'a','b','c',0}` (then the brace-init zeroes
/// `s[4]`/`s[5]`). The NUL is appended, then the list is capped to a constant size,
/// so an exactly-filled array (`U8 s[3] = "abc"`) drops it — matching C. A string
/// initialiser for a *pointer* (`U8 *p = "abc"`) is left alone (a pointer to the
/// literal). Returns `None` when this isn't a string-into-char-array initialiser.
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

/// Relational operators participate in HolyC chained range comparisons
/// (`a < b < c`). Equality (`==`/`!=`) deliberately does not, so `a == b == c`
/// keeps its standard C meaning `(a == b) == c`.
fn is_chain_cmp(op: BinOp) -> bool {
    matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge)
}

/// Map an infix-operator token to its [`BinOp`] and binding power. Higher binds
/// tighter; left-associative operators recurse at `bp + 1`.
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

/// Parse source text into a [`Program`].
///
/// This runs the full front-end pipeline twice over the source (each pass
/// streams; the token list is never fully buffered):
///
///   1. A pre-pass streams the *preprocessed* tokens and hoists every
///      `class`/`union` name, so forward references to a type parse correctly.
///   2. The real parse runs the preprocessor again, seeded with those names.
///
/// Re-running the deterministic preprocessor is cheap and keeps both passes
/// lazy.
/// The canonical name of the tuple type with these element types — a deterministic
/// mangling, so two `(I64, Error)`s anywhere name the same synthetic struct.
fn tuple_type_name(elems: &[Type]) -> String {
    let mut s = String::from("$Tup");
    for t in elems {
        s.push('$');
        s.push_str(&mangle_type(t));
    }
    s
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
        Type::Named(n) => n.clone(),
        Type::Ptr(inner) => format!("P{}", mangle_type(inner)),
        Type::Array(inner, _) => format!("Arr{}", mangle_type(inner)),
        Type::FuncPtr { ret, params } => {
            let mut s = format!("Fp{}", mangle_type(ret));
            for p in params {
                s.push('_');
                s.push_str(&mangle_type(p));
            }
            s
        }
    }
}

/// A side-effect-free lvalue source for an untyped destructuring assignment
/// (`(a, b) = src;`): a variable or a field/constant-index path rooted at one, so
/// re-reading it per slot is safe. A call or other computed expression is not.
fn is_simple_source(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) => true,
        ExprKind::Member { base, .. } => is_simple_source(base),
        ExprKind::Index { base, index } => {
            matches!(index.kind, ExprKind::Int(_)) && is_simple_source(base)
        }
        ExprKind::Unary {
            op: UnOp::Deref,
            expr,
        } => is_simple_source(expr),
        _ => false,
    }
}

pub fn parse(src: &str) -> PResult<Program> {
    parse_in_dir(src, std::path::Path::new("."))
}

/// Parse `src`, resolving `#include "..."` relative to `dir` (the directory of
/// the source file). The CLI passes the input file's parent directory; `parse`
/// defaults it to the current directory. No implicit prelude (the raw front end).
pub fn parse_in_dir(src: &str, dir: &std::path::Path) -> PResult<Program> {
    parse_core(src, dir, &[])
}

/// Parse `src`, resolving `#include "..."` relative to `dir` and angle includes
/// (`#include <name>`) against `search` (the standard-library directories, tried
/// in order). The CLIs pass the input file's parent as `dir` and the stdlib
/// directories as `search`. This is the full front end.
pub fn parse_with(
    src: &str,
    dir: &std::path::Path,
    search: &[std::path::PathBuf],
) -> PResult<Program> {
    parse_core(src, dir, search)
}

fn parse_core(src: &str, dir: &std::path::Path, search: &[std::path::PathBuf]) -> PResult<Program> {
    let known_types = hoist_type_names(src, dir, search)?;
    let pp =
        Preprocessor::with_base_dir_and_search(Lexer::new(src), dir.to_path_buf(), search.to_vec());
    Parser::with_known_types(pp, known_types).parse_program()
}

/// Stream the preprocessed tokens and collect every `class`/`union` name,
/// descending into `#include`d files (so a type defined there can be used).
fn hoist_type_names(
    src: &str,
    dir: &std::path::Path,
    search: &[std::path::PathBuf],
) -> PResult<HashSet<String>> {
    let mut pp =
        Preprocessor::with_base_dir_and_search(Lexer::new(src), dir.to_path_buf(), search.to_vec());
    let mut names = HashSet::new();
    loop {
        let t = pp.next_token()?;
        match t.kind {
            TokenKind::Eof => break,
            TokenKind::Keyword(Keyword::Class) | TokenKind::Keyword(Keyword::Union) => {
                if let TokenKind::Ident(name) = pp.next_token()?.kind {
                    names.insert(name);
                }
            }
            _ => {}
        }
    }
    Ok(names)
}
