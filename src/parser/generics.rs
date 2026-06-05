//! Generic monomorphization, split from `parser.rs`. These methods operate on
//! `Parser` (the state lives in `Parser::generics`, a `Generics`); the templates,
//! instantiation worklists, and call-site type inference all live here. Phase 1 of
//! decoupling inference from parsing — a future type-directed `mono` pass would take
//! the worklist over with full type information.

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

    /// Capture a generic function template's raw tokens (through the body's closing
    /// `}`), register it, and emit nothing. Locates the name token and the
    /// type-parameter list within the captured tokens.
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
        // Extract one type pattern per value parameter (for call-site inference): the
        // value-parameter list runs from `lp` to its matching `)`.
        let mut depth = 0i32;
        let mut rp = lp;
        for (off, t) in toks[lp..].iter().enumerate() {
            match t.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        rp = lp + off;
                        break;
                    }
                }
                _ => {}
            }
        }
        let param_patterns = param_type_patterns(&toks[lp + 1..rp], &type_params);
        self.generics.fns.insert(
            name,
            GenericFn {
                type_params,
                tokens: toks,
                name_index,
                gt_index,
                param_patterns,
            },
        );
        Ok(self.st(StmtKind::Empty, m))
    }

    /// Parse a generic template `class Name<T, …> { … }` (the leading `<` has been
    /// peeked). Records the template for later monomorphization and emits no item.
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
        // Capture the body `{ ... }` as raw tokens (so a field may nest a generic type
        // with this template's parameters); substituted + re-parsed at each instantiation.
        let lb = self.advance()?;
        if !matches!(lb.kind, TokenKind::LBrace) {
            return self.err("expected `{` to begin a generic class body");
        }
        let mut body_tokens = vec![lb];
        let mut depth = 1i32;
        loop {
            let t = self.advance()?;
            match t.kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => depth -= 1,
                TokenKind::Eof => return self.err("unterminated generic class (missing `}`)"),
                _ => {}
            }
            let close = depth == 0;
            body_tokens.push(t);
            if close {
                break;
            }
        }
        self.eat(&TokenKind::Semicolon)?;
        self.generics.classes.insert(
            name,
            GenericClass {
                is_union,
                params,
                base,
                body_tokens,
            },
        );
        Ok(self.st(StmtKind::Empty, m)) // the template itself produces no code
    }

    /// Drain the generic-instantiation worklists to a fixpoint, appending each synthetic
    /// concrete `class`/function to `items`. Generating one instance may request more of
    /// either (a body's nested generic use/call), so this loops until both queues are
    /// empty. Sema collects all types/functions regardless of order, so appending at the
    /// end is fine. (The single entry point for monomorphization — a future type-directed
    /// `mono` pass would replace this.)
    pub(super) fn drain_generics(&mut self, items: &mut Vec<Stmt>) -> PResult<()> {
        let mut guard = 0usize;
        loop {
            guard += 1;
            if guard > 1_000_000 {
                return self.err("generic instantiation did not terminate");
            }
            if let Some((name, type_args)) = self.generics.pending_classes.pop() {
                let mangled = mangle_generic(&name, &type_args);
                let cls = self.instantiate_generic_class(&name, &type_args, &mangled)?;
                items.append(&mut self.pending_types);
                items.push(cls);
            } else if let Some((name, type_args)) = self.generics.pending_fns.pop() {
                let mangled = mangle_generic(&name, &type_args);
                if !self.generics.fn_done.insert(mangled.clone()) {
                    continue;
                }
                let func = self.instantiate_generic_fn(&name, &type_args, &mangled)?;
                items.append(&mut self.pending_types);
                items.push(func);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Use of generic `name` in type position: parse `<arg, …>`, return the concrete
    /// type name now, and queue the instance for generation after the main parse
    /// (deduped by its mangled name).
    pub(super) fn instantiate_generic(&mut self, name: &str, pos: Pos) -> PResult<Type> {
        self.expect(&TokenKind::Lt, "`<` for generic type arguments")?;
        let mut args = Vec::new();
        loop {
            // A nested generic argument (`Vec<Vec<I8>>`) instantiates first, naturally.
            args.push(self.parse_type_no_name()?);
            if !self.eat(&TokenKind::Comma)? {
                break;
            }
        }
        self.expect_generic_gt()?; // closes `>`, splitting a `>>` for nesting
        let tmpl = self
            .generics
            .classes
            .get(name)
            .expect("generic exists")
            .clone();
        if args.len() != tmpl.params.len() {
            return Err(ParseError {
                message: format!(
                    "generic `{name}` expects {} type argument(s), got {}",
                    tmpl.params.len(),
                    args.len()
                ),
                pos,
            });
        }
        let mangled = mangle_generic(name, &args);
        self.generics
            .instances
            .insert(mangled.clone(), (name.to_string(), args.clone()));
        // Return the concrete type name now; generate the class definition after the main
        // parse (so a nested generic field re-types correctly and self-reference dedups).
        if self.generics.class_done.insert(mangled.clone()) {
            self.known_types.insert(mangled.clone());
            self.generics.pending_classes.push((name.to_string(), args));
        }
        Ok(Type::Named(mangled))
    }

    /// Generate a concrete class from a generic class template: substitute the
    /// type-parameter tokens, name it `mangled`, and re-parse the body (so nested
    /// generic fields instantiate concretely and re-queue).
    pub(super) fn instantiate_generic_class(
        &mut self,
        name: &str,
        type_args: &[Type],
        mangled: &str,
    ) -> PResult<Stmt> {
        let tmpl = self
            .generics
            .classes
            .get(name)
            .expect("generic class")
            .clone();
        let kw = if tmpl.is_union {
            Keyword::Union
        } else {
            Keyword::Class
        };
        let mut out = vec![
            Token::new(TokenKind::Keyword(kw), Span::dummy()),
            Token::new(TokenKind::Ident(mangled.to_string()), Span::dummy()),
        ];
        if let Some(base) = &tmpl.base {
            out.push(Token::new(TokenKind::Colon, Span::dummy()));
            out.push(Token::new(TokenKind::Ident(base.clone()), Span::dummy()));
        }
        let subst: HashMap<&str, Vec<Token>> = tmpl
            .params
            .iter()
            .zip(type_args.iter())
            .map(|(p, a)| (p.as_str(), type_to_tokens(a)))
            .collect();
        for t in &tmpl.body_tokens {
            if let TokenKind::Ident(s) = &t.kind {
                if let Some(rep) = subst.get(s.as_str()) {
                    out.extend(rep.iter().cloned());
                    continue;
                }
            }
            out.push(t.clone());
        }
        let saved = std::mem::take(&mut self.buf);
        self.buf = out.into();
        let cm = self.mark()?;
        let cls = self.parse_class(cm);
        self.buf = saved;
        cls
    }

    /// Parse a `<T1, …>` type-argument list (for a generic call).
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

    /// Monomorphize a generic function: substitute the type-parameter tokens with the
    /// argument tokens, drop the `<T>` list, rename to `mangled`, and re-parse the
    /// result into a concrete function definition (sharing this parser's tables, so
    /// generic types/calls in the body resolve and re-queue).
    pub(super) fn instantiate_generic_fn(
        &mut self,
        name: &str,
        type_args: &[Type],
        mangled: &str,
    ) -> PResult<Stmt> {
        let tmpl = self
            .generics
            .fns
            .get(name)
            .expect("generic fn exists")
            .clone();
        if type_args.len() != tmpl.type_params.len() {
            return self.err_at(
                tmpl.tokens[tmpl.name_index].span.pos,
                format!(
                    "generic function `{name}` expects {} type argument(s), got {}",
                    tmpl.type_params.len(),
                    type_args.len()
                ),
            );
        }
        let subst: HashMap<&str, Vec<Token>> = tmpl
            .type_params
            .iter()
            .zip(type_args.iter())
            .map(|(p, a)| (p.as_str(), type_to_tokens(a)))
            .collect();
        let name_tok = Token::new(TokenKind::Ident(mangled.to_string()), Span::dummy());
        let mut out: Vec<Token> = Vec::new();
        for (i, t) in tmpl.tokens.iter().enumerate() {
            // Drop the type-parameter list `<T, …>` (from `<` to its `>`).
            if i > tmpl.name_index && i <= tmpl.gt_index {
                continue;
            }
            if i == tmpl.name_index {
                out.push(name_tok.clone());
                continue;
            }
            if let TokenKind::Ident(s) = &t.kind {
                if let Some(rep) = subst.get(s.as_str()) {
                    out.extend(rep.iter().cloned());
                    continue;
                }
            }
            out.push(t.clone());
        }
        // Re-parse `out` as a concrete function through this same parser (so its body's
        // generic types/calls resolve and contribute to the pending queues).
        let saved = std::mem::take(&mut self.buf);
        self.buf = out.into();
        let dm = self.mark()?;
        let func = self.parse_declaration(dm);
        self.buf = saved;
        func
    }

    /// Infer a generic function's type arguments from a call's argument types, queue
    /// the instance, and return its mangled name. Errors if any type parameter can't
    /// be inferred (the caller should then use explicit `Name<...>(...)`).
    pub(super) fn infer_generic_call(
        &mut self,
        name: &str,
        args: &[Expr],
        pos: Pos,
    ) -> PResult<String> {
        let tmpl = self.generics.fns.get(name).expect("generic fn").clone();
        let mut binds: HashMap<String, Type> = HashMap::new();
        for (i, pat) in tmpl.param_patterns.iter().enumerate() {
            if let Some(arg) = args.get(i) {
                if let Some(aty) = self.arg_type(arg) {
                    self.unify_pattern(pat, &aty, &mut binds);
                }
            }
        }
        let mut type_args = Vec::new();
        for p in &tmpl.type_params {
            match binds.get(p) {
                Some(t) => type_args.push(t.clone()),
                None => {
                    return self.err_at(
                        pos,
                        format!(
                            "cannot infer type argument `{p}` for generic `{name}`; \
                             call it as `{name}<...>(...)`"
                        ),
                    );
                }
            }
        }
        let mangled = mangle_generic(name, &type_args);
        // Record the instance's return type (so a `:=` / call-result inference can see
        // through a generic call like `c, found := HmapGet(...)`, which returns a tuple).
        self.record_generic_ret(&tmpl, &type_args, &mangled);
        self.generics
            .pending_fns
            .push((name.to_string(), type_args));
        Ok(mangled)
    }

    /// Compute a generic-function instance's concrete return type — substitute the bound
    /// type args into the template's return-type tokens (everything before the function
    /// name) and parse them — and record it in `fn_rets`, so call-result inference can
    /// see a generic call's type at parse time. Handles a pointer return (`T *Foo`),
    /// where the `*` sits between the base type and the name. Best-effort: only recorded
    /// when the tokens parse cleanly in full.
    pub(super) fn record_generic_ret(
        &mut self,
        tmpl: &GenericFn,
        type_args: &[Type],
        mangled: &str,
    ) {
        let subst: HashMap<&str, Vec<Token>> = tmpl
            .type_params
            .iter()
            .zip(type_args)
            .map(|(p, a)| (p.as_str(), type_to_tokens(a)))
            .collect();
        let mut out = Vec::new();
        for t in &tmpl.tokens[..tmpl.name_index] {
            if let TokenKind::Ident(s) = &t.kind {
                if let Some(rep) = subst.get(s.as_str()) {
                    out.extend(rep.iter().cloned());
                    continue;
                }
            }
            out.push(t.clone());
        }
        if let Some(ty) = self.parse_type_from_tokens(out) {
            self.generics.fn_rets.insert(mangled.to_string(), ty);
        }
    }

    /// Best-effort static type of a generic call's argument expression, used to infer
    /// type arguments at an un-annotated call. A parse-time mini-typer over the
    /// recording-only maps (`var_types`/`fn_rets`/`class_fields`): it resolves literals,
    /// variables, `&e`, casts, call results, member access, indexing, deref, and
    /// arithmetic. `None` ⇒ can't infer (the call must then give explicit `<...>` type
    /// arguments). It is "seen so far" and intentionally partial — not a full type
    /// checker — so a form it can't resolve simply falls back to the explicit syntax.
    pub(super) fn arg_type(&self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Char(_) => Some(Type::I64),
            ExprKind::Float(_) => Some(Type::F64),
            ExprKind::Str(_) => Some(Type::Ptr(Box::new(Type::U8))),
            ExprKind::Ident(n) => self.generics.var_types.get(n).cloned(),
            ExprKind::Unary {
                op: UnOp::AddrOf,
                expr,
            } => Some(Type::Ptr(Box::new(self.arg_type(expr)?))),
            // `*p` — the pointee type.
            ExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => match self.arg_type(expr)? {
                Type::Ptr(inner) => Some(*inner),
                _ => None,
            },
            // An explicit cast names its own type.
            ExprKind::Cast { ty, .. } => Some(ty.clone()),
            // A call result: the callee's recorded return type (a plain named call only —
            // a function pointer or a not-yet-seen function isn't resolvable here).
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Ident(n) => self.generics.fn_rets.get(n).cloned(),
                _ => None,
            },
            // `b.field` / `p->field` — the field's recorded type (direct fields only;
            // the base may be a class value or a pointer to one).
            ExprKind::Member { base, field, .. } => {
                let cname = match self.arg_type(base)? {
                    Type::Named(n) => n,
                    Type::Ptr(inner) => match *inner {
                        Type::Named(n) => n,
                        _ => return None,
                    },
                    _ => return None,
                };
                self.generics
                    .class_fields
                    .get(&cname)?
                    .iter()
                    .find(|(n, _)| n == field)
                    .map(|(_, t)| t.clone())
            }
            // `a[i]` — the element type (an array or pointer base decays to it).
            ExprKind::Index { base, .. } => match self.arg_type(base)? {
                Type::Ptr(elem) | Type::Array(elem, _) => Some(*elem),
                _ => None,
            },
            // Arithmetic/bitwise: HolyC promotes narrow integers to `I64`, and `F64`
            // dominates if either operand is float; comparisons/logicals are boolean
            // (`I64`). (Mirrors the backends' integer-promotion rule.)
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or => Some(Type::I64),
                _ => {
                    let f = matches!(self.arg_type(lhs), Some(Type::F64))
                        || matches!(self.arg_type(rhs), Some(Type::F64));
                    Some(if f { Type::F64 } else { Type::I64 })
                }
            },
            _ => None,
        }
    }

    /// Match a parameter pattern against an argument type, binding type parameters.
    pub(super) fn unify_pattern(
        &self,
        pat: &TypePattern,
        ty: &Type,
        out: &mut HashMap<String, Type>,
    ) {
        match pat {
            TypePattern::Param(p) => {
                out.entry(p.clone()).or_insert_with(|| ty.clone());
            }
            TypePattern::Ptr(inner) => {
                if let Type::Ptr(t) = ty {
                    self.unify_pattern(inner, t, out);
                }
            }
            TypePattern::Generic(g, pats) => {
                if let Type::Named(n) = ty {
                    if let Some((gname, targs)) = self.generics.instances.get(n) {
                        if gname == g {
                            for (pa, ta) in pats.iter().zip(targs.iter()) {
                                self.unify_pattern(pa, ta, out);
                            }
                        }
                    }
                }
            }
            TypePattern::Concrete => {}
        }
    }
}
