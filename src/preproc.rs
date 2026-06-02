//! The HolyC preprocessor.
//!
//! A [`Preprocessor`] wraps any [`TokenStream`] (normally a [`Lexer`]) and is
//! itself a `TokenStream`, so it slots between the lexer and the parser without
//! ever materialising the whole token list. As tokens flow through it:
//!
//!   * `#define` / `#undef` build and tear down a macro table,
//!   * object-like and function-like macros are expanded inline (with nested
//!     expansion and a hide-set guard against runaway self-reference),
//!   * `#ifdef` / `#ifndef` / `#else` / `#endif` include or drop token ranges,
//!   * `#include "file"` is resolved: the file is read and pushed onto a source
//!     stack so its tokens splice in (relative to the including file's
//!     directory; cycles and runaway nesting are rejected); unknown directives
//!     are dropped.
//!
//! Directives run to the end of their line. The lexer discards newlines, but
//! every token carries `span.pos.line`, so the preprocessor finds line
//! boundaries from token positions — no newline tokens required.
//!
//! Limitations (documented intentionally): no `#if <expr>` (only def-ness
//! conditionals), no `#`/`##` operators, no `__VA_ARGS__`, and macro-argument
//! hide-sets are coarse (good enough to prevent infinite expansion, not fully
//! C-standard).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::lexer::{LexError, Lexer, TokenStream};
use crate::token::{Keyword, Pos, Token, TokenKind};

type LResult<T> = Result<T, LexError>;

/// A hard cap on `#include` nesting, as a backstop beyond the cycle guard.
const MAX_INCLUDE_DEPTH: usize = 64;

/// One open `#include`d file on the source stack.
struct IncludeFrame {
    /// The lexer streaming the included file's tokens.
    lexer: Lexer,
    /// The token already read past the `#include` line in the parent; re-queued
    /// when this file is exhausted so the parent resumes exactly where it left off.
    resume: Option<Token>,
    /// The directory of this file, for resolving its own relative `#include`s.
    dir: PathBuf,
    /// The canonical path of this file, for cycle detection.
    path: PathBuf,
    /// Conditional-nesting depth when this file was entered, so an unterminated
    /// `#ifdef` inside it is caught rather than leaking into the parent.
    cond_depth: usize,
}

#[derive(Clone)]
enum Macro {
    Object(Vec<Token>),
    Func {
        params: Vec<String>,
        body: Vec<Token>,
    },
}

/// A token paired with the set of macro names that must not be re-expanded
/// within it (the classic preprocessor "hide set").
#[derive(Clone)]
struct PpTok {
    tok: Token,
    hide: HashSet<String>,
}

/// One level of conditional nesting.
struct Cond {
    /// Whether the enclosing context was active when this `#ifdef` was seen.
    parent_eff: bool,
    /// Whether the current branch is active (and emits tokens).
    eff: bool,
    /// Whether any branch at this level has been taken yet.
    any_taken: bool,
    /// Whether `#else` has already appeared at this level.
    seen_else: bool,
}

pub struct Preprocessor<S: TokenStream> {
    inner: S,
    /// One-token push-back for the inner stream (used when a directive reads one
    /// token past its line).
    lookahead: Option<Token>,
    /// Buffered/expanded tokens awaiting output, nearest first.
    pending: VecDeque<PpTok>,
    macros: HashMap<String, Macro>,
    conds: Vec<Cond>,
    /// Set once we've reported an unterminated-conditional error, to avoid
    /// repeating it on every subsequent Eof read.
    eof_reported: bool,
    /// Directory the top-level source was read from, for resolving its relative
    /// `#include "..."` paths.
    base_dir: PathBuf,
    /// Standard-library search directories for **angle** includes
    /// (`#include <math.hc>`), tried in order. Quote includes ignore these.
    search: Vec<PathBuf>,
    /// The stack of currently-open `#include`d files (innermost last). Tokens are
    /// pulled from the top of this stack before the base `inner` stream.
    includes: Vec<IncludeFrame>,
}

impl<S: TokenStream> Preprocessor<S> {
    pub fn new(inner: S) -> Self {
        Self::with_base_dir(inner, PathBuf::from("."))
    }

    /// Build a preprocessor that resolves relative `#include "..."` paths against
    /// `base_dir` (the directory of the top-level source file).
    pub fn with_base_dir(inner: S, base_dir: PathBuf) -> Self {
        Self::with_base_dir_and_search(inner, base_dir, Vec::new())
    }

    /// As [`with_base_dir`](Self::with_base_dir), plus a list of search
    /// directories for **angle** includes (`#include <name>`) — the standard
    /// library. Each is tried in order; the first that holds the file wins.
    pub fn with_base_dir_and_search(inner: S, base_dir: PathBuf, search: Vec<PathBuf>) -> Self {
        Preprocessor {
            inner,
            lookahead: None,
            pending: VecDeque::new(),
            macros: HashMap::new(),
            conds: Vec::new(),
            eof_reported: false,
            base_dir,
            search,
            includes: Vec::new(),
        }
    }

    fn err(&self, pos: Pos, msg: impl Into<String>) -> LexError {
        LexError {
            message: format!("preprocessor: {}", msg.into()),
            pos,
        }
    }

    fn active(&self) -> bool {
        self.conds.last().map(|c| c.eff).unwrap_or(true)
    }

    fn inner_next(&mut self) -> LResult<Token> {
        if let Some(t) = self.lookahead.take() {
            return Ok(t);
        }
        // Tokens come from the innermost open `#include` first, then the base
        // source. A frame's Eof is surfaced to `pull`, which pops the frame.
        if let Some(frame) = self.includes.last_mut() {
            return frame.lexer.next_token();
        }
        self.inner.next_token()
    }

    // ---- layer A: directives & conditionals, no macro expansion ----

    /// Pull the next token with directives handled and inactive branches
    /// skipped. Macro names come through unexpanded.
    fn pull(&mut self) -> LResult<Token> {
        loop {
            let t = self.inner_next()?;
            match &t.kind {
                TokenKind::Eof => {
                    // An included file ended: pop its frame, check its conditionals
                    // were balanced, and resume the parent stream.
                    if let Some(frame) = self.includes.pop() {
                        if self.conds.len() != frame.cond_depth {
                            self.conds.truncate(frame.cond_depth);
                            return Err(self.err(
                                t.span.pos,
                                "unterminated #ifdef/#ifndef in included file (missing #endif)",
                            ));
                        }
                        self.lookahead = frame.resume;
                        continue;
                    }
                    if !self.conds.is_empty() && !self.eof_reported {
                        self.eof_reported = true;
                        return Err(
                            self.err(t.span.pos, "unterminated #ifdef/#ifndef (missing #endif)")
                        );
                    }
                    return Ok(t);
                }
                TokenKind::Hash => self.directive(t)?,
                _ => {
                    if self.active() {
                        return Ok(t);
                    }
                    // Otherwise drop this token (inactive conditional branch).
                }
            }
        }
    }

    /// Handle a directive line introduced by `hash`.
    fn directive(&mut self, hash: Token) -> LResult<()> {
        let line = hash.span.pos.line;
        let mut toks = Vec::new();
        loop {
            let t = self.inner_next()?;
            if matches!(t.kind, TokenKind::Eof) || t.span.pos.line != line {
                self.lookahead = Some(t); // belongs to the next line
                break;
            }
            toks.push(t);
        }
        if toks.is_empty() {
            return Ok(()); // a lone `#`
        }

        match directive_name(&toks[0]).as_deref() {
            // Conditionals are processed even inside an inactive branch, so
            // nesting stays balanced.
            Some("ifdef") => self.do_ifdef(&toks, true),
            Some("ifndef") => self.do_ifdef(&toks, false),
            Some("else") => self.do_else(hash.span.pos),
            Some("endif") => self.do_endif(hash.span.pos),

            // Everything else is ignored while inactive.
            _ if !self.active() => Ok(()),

            Some("define") => self.do_define(&toks),
            Some("undef") => self.do_undef(&toks),
            Some("include") => self.do_include(&toks),
            // Unknown directive (e.g. `#help_index`): drop it.
            _ => Ok(()),
        }
    }

    fn do_define(&mut self, toks: &[Token]) -> LResult<()> {
        let name = match toks.get(1).map(|t| &t.kind) {
            Some(TokenKind::Ident(s)) => s.clone(),
            Some(_) => return Err(self.err(toks[1].span.pos, "macro name must be an identifier")),
            None => return Err(self.err(toks[0].span.pos, "#define is missing a macro name")),
        };

        // Function-like only when `(` immediately follows the name with no gap.
        if let Some(lparen) = toks.get(2) {
            if matches!(lparen.kind, TokenKind::LParen) && toks[1].span.end == lparen.span.start {
                let (params, body_start) = self.parse_macro_params(toks)?;
                let body = toks[body_start..].to_vec();
                self.macros.insert(name, Macro::Func { params, body });
                return Ok(());
            }
        }

        let body = toks[2..].to_vec();
        self.macros.insert(name, Macro::Object(body));
        Ok(())
    }

    /// Parse a function-like macro's parameter list, starting at the `(` in
    /// `toks[2]`. Returns the parameter names and the index where the body
    /// begins.
    fn parse_macro_params(&self, toks: &[Token]) -> LResult<(Vec<String>, usize)> {
        let mut params = Vec::new();
        let mut i = 3; // past name and `(`
        // Empty list: `NAME()`.
        if matches!(toks.get(i).map(|t| &t.kind), Some(TokenKind::RParen)) {
            return Ok((params, i + 1));
        }
        loop {
            match toks.get(i).map(|t| &t.kind) {
                Some(TokenKind::Ident(p)) => params.push(p.clone()),
                _ => {
                    let pos = toks.get(i).map(|t| t.span.pos).unwrap_or(toks[0].span.pos);
                    return Err(self.err(pos, "expected a macro parameter name"));
                }
            }
            i += 1;
            match toks.get(i).map(|t| &t.kind) {
                Some(TokenKind::Comma) => i += 1,
                Some(TokenKind::RParen) => return Ok((params, i + 1)),
                _ => {
                    let pos = toks.get(i).map(|t| t.span.pos).unwrap_or(toks[0].span.pos);
                    return Err(self.err(pos, "expected `,` or `)` in macro parameter list"));
                }
            }
        }
    }

    fn do_undef(&mut self, toks: &[Token]) -> LResult<()> {
        match toks.get(1).map(|t| &t.kind) {
            Some(TokenKind::Ident(s)) => {
                self.macros.remove(s);
                Ok(())
            }
            _ => Err(self.err(toks[0].span.pos, "#undef is missing a macro name")),
        }
    }

    /// Resolve and open a `#include "path"`: read the file and push it onto the
    /// source stack so its tokens stream in next. The path is resolved relative
    /// to the directory of the file containing the directive; cycles and runaway
    /// nesting are rejected.
    fn do_include(&mut self, toks: &[Token]) -> LResult<()> {
        let pos = toks[0].span.pos;
        // Two forms: `#include "file"` (a single string token, resolved relative
        // to the including file) and `#include <name>` (an angle path spelled as
        // separate tokens, resolved against the standard-library search path).
        match toks.get(1).map(|t| &t.kind) {
            Some(TokenKind::Str(p)) => {
                let path_str = p.clone();
                let cur_dir = self
                    .includes
                    .last()
                    .map(|f| f.dir.clone())
                    .unwrap_or_else(|| self.base_dir.clone());
                let canon = cur_dir.join(&path_str).canonicalize().map_err(|e| {
                    self.err(pos, format!("cannot open #include \"{path_str}\": {e}"))
                })?;
                self.open_include(canon, &format!("\"{path_str}\""), pos)
            }
            Some(TokenKind::Lt) => {
                let path_str = angle_path(&toks[1..])
                    .ok_or_else(|| self.err(pos, "malformed #include <...> path"))?;
                // First search directory that holds the file wins.
                let canon = self
                    .search
                    .iter()
                    .find_map(|d| d.join(&path_str).canonicalize().ok())
                    .ok_or_else(|| {
                        self.err(
                            pos,
                            format!(
                                "cannot find #include <{path_str}> in the standard-library \
                                 search path (set SOLOMON_STDLIB or pass -I)"
                            ),
                        )
                    })?;
                self.open_include(canon, &format!("<{path_str}>"), pos)
            }
            _ => Err(self.err(pos, "#include expects \"file\" or <name>")),
        }
    }

    /// Push the already-resolved canonical include path onto the source stack
    /// (after the cycle and depth checks). `display` is the original spelling, for
    /// error messages.
    fn open_include(&mut self, canon: PathBuf, display: &str, pos: Pos) -> LResult<()> {
        if self.includes.iter().any(|f| f.path == canon) {
            return Err(self.err(pos, format!("recursive #include of {display}")));
        }
        if self.includes.len() >= MAX_INCLUDE_DEPTH {
            return Err(self.err(pos, "#include nested too deeply"));
        }
        let contents = std::fs::read_to_string(&canon)
            .map_err(|e| self.err(pos, format!("cannot read #include {display}: {e}")))?;
        let dir = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        // The token already read past the `#include` line resumes the parent
        // once the included file is exhausted.
        let resume = self.lookahead.take();
        self.includes.push(IncludeFrame {
            lexer: Lexer::new(&contents),
            resume,
            dir,
            path: canon,
            cond_depth: self.conds.len(),
        });
        Ok(())
    }

    fn do_ifdef(&mut self, toks: &[Token], want_defined: bool) -> LResult<()> {
        let name = match toks.get(1).map(|t| &t.kind) {
            Some(TokenKind::Ident(s)) => s.clone(),
            _ => return Err(self.err(toks[0].span.pos, "#ifdef/#ifndef is missing a name")),
        };
        let parent = self.active();
        let defined = self.macros.contains_key(&name);
        let branch = parent && (defined == want_defined);
        self.conds.push(Cond {
            parent_eff: parent,
            eff: branch,
            any_taken: branch,
            seen_else: false,
        });
        Ok(())
    }

    fn do_else(&mut self, pos: Pos) -> LResult<()> {
        match self.conds.last_mut() {
            None => Err(self.err(pos, "#else without a matching #ifdef")),
            Some(c) if c.seen_else => Err(self.err(pos, "duplicate #else")),
            Some(c) => {
                c.seen_else = true;
                let branch = c.parent_eff && !c.any_taken;
                c.eff = branch;
                c.any_taken = c.any_taken || branch;
                Ok(())
            }
        }
    }

    fn do_endif(&mut self, pos: Pos) -> LResult<()> {
        if self.conds.pop().is_none() {
            return Err(self.err(pos, "#endif without a matching #ifdef"));
        }
        Ok(())
    }

    // ---- layer B: macro expansion ----

    fn ensure_pending(&mut self) -> LResult<()> {
        if self.pending.is_empty() {
            let t = self.pull()?;
            self.pending.push_back(PpTok {
                tok: t,
                hide: HashSet::new(),
            });
        }
        Ok(())
    }

    fn take(&mut self) -> LResult<PpTok> {
        self.ensure_pending()?;
        Ok(self.pending.pop_front().unwrap())
    }

    fn peek_kind(&mut self) -> LResult<TokenKind> {
        self.ensure_pending()?;
        Ok(self.pending.front().unwrap().tok.kind.clone())
    }

    /// The fully-expanded next token.
    fn next_expanded(&mut self) -> LResult<Token> {
        loop {
            let pt = self.take()?;
            if let TokenKind::Ident(name) = &pt.tok.kind {
                let name = name.clone();
                if !pt.hide.contains(&name) {
                    if let Some(mac) = self.macros.get(&name).cloned() {
                        match mac {
                            Macro::Object(body) => {
                                self.expand_into_pending(body, &name, &pt.hide);
                                continue;
                            }
                            Macro::Func { params, body } => {
                                if matches!(self.peek_kind()?, TokenKind::LParen) {
                                    let args = self.collect_args(pt.tok.span.pos)?;
                                    let body = self.substitute(
                                        &params,
                                        &body,
                                        args,
                                        &name,
                                        pt.tok.span.pos,
                                    )?;
                                    self.expand_into_pending(body, &name, &pt.hide);
                                    continue;
                                }
                                // A function-macro name not followed by `(` is
                                // just an ordinary identifier.
                                return Ok(pt.tok);
                            }
                        }
                    }
                }
            }
            return Ok(pt.tok);
        }
    }

    /// Push replacement tokens to the front of the pending queue, each carrying
    /// the current hide-set plus `mac` to prevent re-expanding `mac`.
    fn expand_into_pending(&mut self, body: Vec<Token>, mac: &str, base_hide: &HashSet<String>) {
        let mut hide = base_hide.clone();
        hide.insert(mac.to_string());
        for tok in body.into_iter().rev() {
            self.pending.push_front(PpTok {
                tok,
                hide: hide.clone(),
            });
        }
    }

    /// Collect a function-like macro's arguments. Assumes the next token is `(`.
    fn collect_args(&mut self, pos: Pos) -> LResult<Vec<Vec<Token>>> {
        self.take()?; // consume `(`
        let mut args: Vec<Vec<Token>> = Vec::new();
        let mut cur: Vec<Token> = Vec::new();
        let mut depth = 1usize;
        loop {
            let pt = self.take()?;
            match &pt.tok.kind {
                TokenKind::Eof => return Err(self.err(pos, "unterminated macro argument list")),
                TokenKind::LParen => {
                    depth += 1;
                    cur.push(pt.tok);
                }
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    cur.push(pt.tok);
                }
                TokenKind::Comma if depth == 1 => {
                    args.push(std::mem::take(&mut cur));
                }
                _ => cur.push(pt.tok),
            }
        }
        // The final argument (everything between the last comma and `)`). A bare
        // `()` yields no arguments.
        if !cur.is_empty() || !args.is_empty() {
            args.push(cur);
        }
        Ok(args)
    }

    fn substitute(
        &self,
        params: &[String],
        body: &[Token],
        mut args: Vec<Vec<Token>>,
        name: &str,
        pos: Pos,
    ) -> LResult<Vec<Token>> {
        // A single-parameter macro invoked as `NAME()` passes one empty arg.
        if params.len() == 1 && args.is_empty() {
            args.push(Vec::new());
        }
        if args.len() != params.len() {
            return Err(self.err(
                pos,
                format!(
                    "macro `{name}` expects {} argument(s), got {}",
                    params.len(),
                    args.len()
                ),
            ));
        }
        let mut out = Vec::new();
        for tok in body {
            if let TokenKind::Ident(s) = &tok.kind {
                if let Some(idx) = params.iter().position(|p| p == s) {
                    out.extend(args[idx].iter().cloned());
                    continue;
                }
            }
            out.push(tok.clone());
        }
        Ok(out)
    }
}

impl<S: TokenStream> TokenStream for Preprocessor<S> {
    fn next_token(&mut self) -> LResult<Token> {
        self.next_expanded()
    }
}

/// The directive keyword after `#`. Most directive names lex as identifiers;
/// `else` is the lone keyword among them.
fn directive_name(tok: &Token) -> Option<String> {
    match &tok.kind {
        TokenKind::Ident(s) => Some(s.clone()),
        TokenKind::Keyword(Keyword::Else) => Some("else".to_string()),
        _ => None,
    }
}

/// Reconstruct an angle-include path from the tokens of `#include <name>` (passed
/// starting at the opening `<`). A path has no embedded whitespace and is spelled
/// with the limited filename charset — identifiers, `.`, `/`, `-`, digits — so the
/// tokens between `<` and the first `>` must be adjacent and map to that text.
/// Returns `None` on any gap or unexpected token.
fn angle_path(toks: &[Token]) -> Option<String> {
    if !matches!(toks.first().map(|t| &t.kind), Some(TokenKind::Lt)) {
        return None;
    }
    let close = toks.iter().position(|t| matches!(t.kind, TokenKind::Gt))?;
    let inner = &toks[1..close];
    if inner.is_empty() {
        return None;
    }
    let mut s = String::new();
    for (i, t) in inner.iter().enumerate() {
        if i > 0 && inner[i - 1].span.end != t.span.start {
            return None; // a gap means embedded whitespace — not a path
        }
        match &t.kind {
            TokenKind::Ident(x) => s.push_str(x),
            TokenKind::Int(n) => s.push_str(&n.to_string()),
            TokenKind::Dot => s.push('.'),
            TokenKind::Slash => s.push('/'),
            TokenKind::Minus => s.push('-'),
            _ => return None,
        }
    }
    Some(s)
}
