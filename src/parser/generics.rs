//! The parser's generic-template handling, split from `parser.rs`. These methods
//! only **recognize and capture** generic templates and the angle-bracket grammar —
//! they never instantiate anything. A generic *use* (a type `Vec<I64>` or a call
//! `Id<I64>(x)`) is left as a deferred AST node (`Type::Generic` / `Type::Tuple` /
//! `ExprKind::GenericCall`), and the [`mono`](crate::mono) pass does all
//! monomorphization and type-directed inference after the parse.

use super::*;

impl<S: TokenStream> Parser<S> {
    /// Consume a generic-closing `>`. A `>>` (`Shr`) token is split into two `>` —
    /// the first closes this level, the second is pushed back for the enclosing one —
    /// so nested `Vec<Vec<I8>>` parses (the classic angle-bracket problem).
    pub(super) fn expect_generic_gt(&mut self) -> PResult<()> {
        match self.peek_kind()? {
            TokenKind::Gt => {
                self.advance()?;
                Ok(())
            }
            TokenKind::Shr => {
                let t = self.advance()?;
                let mut sp = t.span;
                sp.start += 1; // the leftover second `>`
                self.buf.push_front(Token::new(TokenKind::Gt, sp));
                Ok(())
            }
            _ => self.err("expected `>` to close generic type arguments"),
        }
    }

    /// Skip a balanced `<…>` starting at peek index `start` (which must be `<`),
    /// returning the index just past the matching `>` (or `>>`). Returns 0 if
    /// unbalanced (run off the end). Used only for non-consuming look-ahead.
    pub(super) fn skip_angle(&mut self, start: usize) -> PResult<usize> {
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
    /// be the `(`), returning the offset just past the matching `)`, or 0 on EOF.
    pub(super) fn skip_parens(&mut self, start: usize) -> PResult<usize> {
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
    /// definition `Ret Name<…>(`? (A `<` immediately after the function name is the
    /// signal — no existing declaration has that shape, so this never misfires.)
    pub(super) fn looks_like_generic_fn(&mut self) -> PResult<bool> {
        let mut i = 0;
        // Return type: a type keyword, or any identifier (a class name, or one of the
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
        // The function name, then `<` (the type-parameter list) then eventually `(`.
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
    /// type parameters left symbolic (`T` → `Type::Param`, nested `Vec<T>` → a deferred
    /// `Type::Generic`, a body generic call → `ExprKind::GenericCall`, a `:=` →
    /// `StmtKind::ShortDecl`). Registers the template and emits nothing; the
    /// [`mono`](crate::mono) pass substitutes the parameters at each instantiation.
    pub(super) fn capture_generic_fn(&mut self, m: Mark) -> PResult<Stmt> {
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
        // closes the type-parameter list (not, say, a `(…)` tuple *return* type, whose
        // `(` comes first); the matching `<` follows the function name.
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
        let mut type_params = Vec::new();
        for t in &toks[lt_index + 1..gt_index] {
            if let TokenKind::Ident(s) = &t.kind {
                type_params.push(s.clone());
            }
        }
        // Register a placeholder first so a self-recursive generic call in the body is
        // recognized as generic (and thus deferred) while parsing the template. The
        // `mono` pass infers type arguments from the parsed parameter types directly.
        self.generics.fns.insert(
            name.clone(),
            GenericFn {
                type_params: type_params.clone(),
                def: FuncDef {
                    ret: Type::U0,
                    name: name.clone(),
                    params: Vec::new(),
                    varargs: false,
                    body: None,
                },
            },
        );
        // Parse the template once into a `FuncDef` with its parameters symbolic: drop the
        // `<T,…>` list, then parse under template scope.
        let mut dropped: Vec<Token> = toks
            .iter()
            .enumerate()
            .filter(|(i, _)| !(*i > name_index && *i <= gt_index))
            .map(|(_, t)| t.clone())
            .collect();
        dropped.push(Token::new(TokenKind::Eof, Span::dummy()));
        let saved_buf = std::mem::take(&mut self.buf);
        let saved_tp = self.template_params.replace(type_params);
        self.buf = dropped.into();
        let dm = self.mark()?;
        let parsed = self.parse_declaration(dm);
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

    /// Parse a generic template `class Name<T, …> { … }` (the leading `<` has been
    /// peeked). Records the template for the [`mono`](crate::mono) pass and emits no
    /// item.
    pub(super) fn parse_generic_class(
        &mut self,
        is_union: bool,
        name: String,
        m: Mark,
    ) -> PResult<Stmt> {
        self.advance()?; // `<`
        let mut params = Vec::new();
        loop {
            params.push(self.expect_ident()?);
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
        // self-referential field — `HmapEntry<K,V> *next` inside `class HmapEntry<K,V>`
        // — recognizes the name as generic and defers it.
        self.generics.classes.insert(
            name.clone(),
            GenericClass {
                is_union,
                params: params.clone(),
                base: base.clone(),
                fields: Vec::new(),
            },
        );
        // Parse the body with the parameters in scope, so field types keep them symbolic
        // (`Type::Param` / deferred `Type::Generic`) for `mono` to substitute.
        let saved = self.template_params.replace(params);
        let fields = self.parse_class_fields();
        self.template_params = saved;
        let fields = fields?;
        self.eat(&TokenKind::Semicolon)?;
        if let Some(tmpl) = self.generics.classes.get_mut(&name) {
            tmpl.fields = fields;
        }
        Ok(self.st(StmtKind::Empty, m)) // the template itself produces no code
    }

    /// Parse a `<T1, …>` type-argument list (for a generic type use or call). The
    /// arguments stay as parsed `Type`s (possibly themselves deferred `Type::Generic`);
    /// `mono` resolves them.
    pub(super) fn parse_type_args(&mut self) -> PResult<Vec<Type>> {
        self.expect(&TokenKind::Lt, "`<` for type arguments")?;
        let mut args = Vec::new();
        loop {
            args.push(self.parse_type_no_name()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect_generic_gt()?;
        Ok(args)
    }
}
