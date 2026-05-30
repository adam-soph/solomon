//! Recursive-descent parser for HolyC.
//!
//! The parser is constructed from a [`Lexer`] and pulls tokens lazily via
//! [`Lexer::next_token`]. It keeps only a tiny look-ahead buffer (a couple of
//! tokens), so the complete token stream is never held in memory at once.
//!
//! Every node it produces carries a [`Span`]. A node's span runs from the start
//! of its first token to the end of its last token; the parser tracks the end
//! of the most recently consumed token in `prev_end`.

use std::collections::{HashSet, VecDeque};
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
}

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
        }
    }

    /// Parse a whole translation unit.
    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof)? {
            items.push(self.parse_stmt()?);
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
        Ok(self.kind_is_type_start(&k))
    }

    /// Parse a base type name (`I64`, a class name, ...). Pointers and arrays
    /// are applied by [`Self::parse_declarator`], not here.
    fn parse_base_type(&mut self) -> PResult<Type> {
        let t = self.advance()?;
        match t.kind {
            TokenKind::Keyword(kw) => Type::from_keyword(kw).ok_or_else(|| ParseError {
                message: format!("`{}` is not a type", kw.as_str()),
                pos: t.span.pos,
            }),
            TokenKind::Ident(s) => Ok(Type::Named(s)),
            other => Err(ParseError {
                message: format!("expected a type, found {other:?}"),
                pos: t.span.pos,
            }),
        }
    }

    /// Parse `*`… `name` `[dim]`… given a base type, returning the declared name
    /// and its fully built type.
    fn parse_declarator(&mut self, base: &Type) -> PResult<(String, Type)> {
        let mut ty = base.clone();
        while self.eat(&TokenKind::Star)? {
            ty = Type::Ptr(Box::new(ty));
        }
        let name = self.expect_ident()?;
        ty = self.parse_array_suffix(ty)?;
        Ok((name, ty))
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
        let m = self.mark()?;

        // Label: `name:` (but not `name::`, which is a scope operator).
        if matches!(self.peek()?.kind, TokenKind::Ident(_))
            && self.peek_n(1)?.kind == TokenKind::Colon
        {
            let name = self.expect_ident()?;
            self.advance()?; // ':'
            return Ok(self.st(StmtKind::Label(name), m));
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
            Some(self.parse_expr()?)
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
        let init = if self.eat(&TokenKind::Eq)? {
            Some(self.parse_assign()?)
        } else {
            None
        };
        Ok(Declarator {
            name,
            ty,
            init,
            span: self.finish(m),
        })
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
                // The parameter name is optional (prototypes may omit it).
                let name = if matches!(self.peek()?.kind, TokenKind::Ident(_)) {
                    Some(self.expect_ident()?)
                } else {
                    None
                };
                ty = self.parse_array_suffix(ty)?;
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

        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace)? {
            if self.at(&TokenKind::Eof)? {
                return self.err("unexpected end of input in class body, expected `}`");
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
            lhs = self.ex(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                m,
            );
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
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
                // Parenthesised expression; keep the inner node's own span.
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                return Ok(e);
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
pub fn parse(src: &str) -> PResult<Program> {
    let known_types = hoist_type_names(src)?;
    let pp = Preprocessor::new(Lexer::new(src));
    Parser::with_known_types(pp, known_types).parse_program()
}

/// Stream the preprocessed tokens and collect every `class`/`union` name.
fn hoist_type_names(src: &str) -> PResult<HashSet<String>> {
    let mut pp = Preprocessor::new(Lexer::new(src));
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
