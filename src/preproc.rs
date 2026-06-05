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
//!   * `#exe { ... }` runs the block as HolyC at **compile time** (via the
//!     interpreter) and pushes its stdout onto the same source stack, so the
//!     generated text streams in where `#exe` appeared (TempleOS-style compile-time
//!     code execution).
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
use std::path::{Path, PathBuf};

use crate::lexer::{LexError, Lexer, TokenStream};
use crate::token::{FileInfo, Keyword, Pos, Token, TokenKind};

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
    /// Index into `Preprocessor::files`, stamped onto every token this frame emits
    /// so `_`-directory privacy can be checked in sema.
    file_id: u32,
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
    /// Append-only table of every source file seen, indexed by `Span::file`
    /// (entry 0 is the base/top-level source). Carries each file's directory for
    /// `_`-directory privacy. Never shrinks, so ids stay valid for the whole parse.
    files: Vec<FileInfo>,
    /// Counter for `#exe` compile-time blocks, so each generated frame gets a unique
    /// synthetic path (the include cycle check keys on the path).
    exe_count: usize,
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

    /// Stream a synthetic prelude ahead of the base source, so its declarations are
    /// in scope without an explicit `#include` (used for `builtin.hc`). The prelude
    /// is the first frame on the include stack — its tokens come first and carry
    /// their own line numbers — and is a **public** file (privacy-wise), so its
    /// builtins are callable from user code.
    pub fn with_prelude(mut self, contents: &str) -> Self {
        let file_id = self.files.len() as u32;
        self.files.push(FileInfo::root());
        self.includes.push(IncludeFrame {
            lexer: Lexer::new(contents),
            resume: None,
            dir: self.base_dir.clone(),
            path: PathBuf::from("<builtin>"),
            cond_depth: self.conds.len(),
            file_id,
        });
        self
    }

    /// As [`with_base_dir`](Self::with_base_dir), plus a list of search
    /// directories for **angle** includes (`#include <name>`) — the standard
    /// library. Each is tried in order; the first that holds the file wins.
    pub fn with_base_dir_and_search(inner: S, base_dir: PathBuf, search: Vec<PathBuf>) -> Self {
        // File 0 is the base/top-level source; its privacy comes from `base_dir`.
        // Canonicalize it so its directory components line up with the canonicalized
        // paths of `#include`d files (otherwise `/tmp` vs `/private/tmp` mismatch).
        let canon_base = base_dir.canonicalize().unwrap_or_else(|_| base_dir.clone());
        let base_info = FileInfo::from_dir(dir_components(&canon_base));
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
            files: vec![base_info],
            exe_count: 0,
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
        // source. A frame's Eof is surfaced to `pull`, which pops the frame. Each
        // frame stamps its origin onto the tokens it emits (the base source is the
        // user program, so its lexer's default `User` is correct).
        if let Some(frame) = self.includes.last_mut() {
            let file_id = frame.file_id;
            let mut t = frame.lexer.next_token()?;
            t.span.file = file_id;
            return Ok(t);
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
        // `#exe { ... }` is special: its block spans lines and braces, so it reads its
        // own brace-balanced body from the stream rather than the line-based collection.
        let first = self.inner_next()?;
        let same_line = !matches!(first.kind, TokenKind::Eof) && first.span.pos.line == line;
        if same_line {
            if let TokenKind::Ident(name) = &first.kind {
                if name == "exe" {
                    return self.do_exe(hash.span.pos);
                }
            }
        }
        let mut toks = Vec::new();
        if same_line {
            toks.push(first);
        } else {
            self.lookahead = Some(first); // belongs to the next line (or Eof)
        }
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

    /// `#exe { … }` — run the block as HolyC at **compile time** and splice its stdout
    /// back into the token stream (TempleOS's compile-time-execution directive). The
    /// block's brace-balanced body is read from the stream, reconstructed to source,
    /// parsed + run through the interpreter, and its output pushed as a source frame
    /// (the `#include` machinery), so it streams in exactly where `#exe` appeared.
    fn do_exe(&mut self, pos: Pos) -> LResult<()> {
        // Expect the opening `{`.
        let open = self.inner_next()?;
        if !matches!(open.kind, TokenKind::LBrace) {
            return Err(self.err(open.span.pos, "#exe must be followed by `{`"));
        }
        // Collect the body tokens up to the matching `}` (strings/chars are single
        // tokens, so brace counting over tokens is safe).
        let mut body = Vec::new();
        let mut depth = 1i32;
        loop {
            let t = self.inner_next()?;
            match t.kind {
                TokenKind::Eof => {
                    return Err(self.err(pos, "unterminated #exe block (missing `}`)"));
                }
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            body.push(t);
        }
        if !self.active() {
            return Ok(()); // inside an inactive #ifdef branch: consumed, not run
        }
        // Reconstruct source, run it, capture stdout.
        let src = render_tokens(&body);
        let program = crate::parser::parse(&src)
            .map_err(|e| self.err(pos, format!("#exe block failed to parse: {e}")))?;
        let out = crate::interp::run_to_string(&program)
            .map_err(|e| self.err(pos, format!("#exe block failed at runtime: {e}")))?;
        // Splice the generated source as a frame (a public file, privacy-wise).
        self.exe_count += 1;
        let path = PathBuf::from(format!("<exe#{}>", self.exe_count));
        self.push_frame(
            path,
            self.base_dir.clone(),
            out,
            "<exe>",
            pos,
            FileInfo::root(),
        )
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
        // Each file's `_`-directory privacy comes from its *own* path (Go-style: no
        // inheritance), computed where the path is resolved below.
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
                let info = file_info_for_disk(&canon);
                self.open_include(canon, &format!("\"{path_str}\""), pos, info)
            }
            Some(TokenKind::Lt) => {
                let path_str = angle_path(&toks[1..])
                    .ok_or_else(|| self.err(pos, "malformed #include <...> path"))?;
                let display = format!("<{path_str}>");
                // 1. The filesystem search path wins, so `SOLOMON_STDLIB`/`-I` can
                //    override the bundled stdlib or add angle-includable files.
                if let Some(canon) = self
                    .search
                    .iter()
                    .find_map(|d| d.join(&path_str).canonicalize().ok())
                {
                    let info = file_info_for_disk(&canon);
                    return self.open_include(canon, &display, pos, info);
                }
                // 2. Otherwise the standard library embedded in the compiler at build
                //    time (so no `lib/` on disk is required).
                if let Some(src) = crate::embedded_stdlib(&path_str) {
                    let path = PathBuf::from(format!("<stdlib:{path_str}>"));
                    let info = file_info_for_embedded(&path_str);
                    return self.push_frame(
                        path,
                        self.base_dir.clone(),
                        src.to_string(),
                        &display,
                        pos,
                        info,
                    );
                }
                Err(self.err(
                    pos,
                    format!(
                        "cannot find #include <{path_str}> in the search path or the embedded \
                         stdlib (set SOLOMON_STDLIB or pass -I)"
                    ),
                ))
            }
            _ => Err(self.err(pos, "#include expects \"file\" or <name>")),
        }
    }

    /// Push the already-resolved canonical include path onto the source stack
    /// (after the cycle and depth checks). `display` is the original spelling, for
    /// error messages.
    fn open_include(
        &mut self,
        canon: PathBuf,
        display: &str,
        pos: Pos,
        info: FileInfo,
    ) -> LResult<()> {
        let contents = std::fs::read_to_string(&canon)
            .map_err(|e| self.err(pos, format!("cannot read #include {display}: {e}")))?;
        let dir = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        self.push_frame(canon, dir, contents, display, pos, info)
    }

    /// Push an include source (read from disk or supplied from the embedded stdlib)
    /// onto the source stack, after the cycle and depth checks. `path` identifies the
    /// frame for the cycle check (a real canonical path, or a `<stdlib:name>`
    /// sentinel); `dir` is the base for that file's relative (`"..."`) includes.
    fn push_frame(
        &mut self,
        path: PathBuf,
        dir: PathBuf,
        contents: String,
        display: &str,
        pos: Pos,
        info: FileInfo,
    ) -> LResult<()> {
        if self.includes.iter().any(|f| f.path == path) {
            return Err(self.err(pos, format!("recursive #include of {display}")));
        }
        if self.includes.len() >= MAX_INCLUDE_DEPTH {
            return Err(self.err(pos, "#include nested too deeply"));
        }
        // The token already read past the `#include` line resumes the parent once the
        // included file is exhausted.
        let resume = self.lookahead.take();
        // Register this file in the (append-only) table; its id is stamped onto the
        // frame's tokens. The table never shrinks when frames pop, so ids stay valid
        // for the whole parse.
        let file_id = self.files.len() as u32;
        self.files.push(info);
        self.includes.push(IncludeFrame {
            lexer: Lexer::new(&contents),
            resume,
            dir,
            path,
            cond_depth: self.conds.len(),
            file_id,
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

    fn source_files(&self) -> Vec<FileInfo> {
        self.files.clone()
    }
}

/// The directive keyword after `#`. Most directive names lex as identifiers;
/// `else` is the lone keyword among them.
/// Reconstruct HolyC source text from a token slice (for re-parsing an `#exe` block).
/// Tokens are space-joined; literals are rendered faithfully — strings re-escaped,
/// floats with a decimal point, char constants as their packed integer value.
fn render_tokens(toks: &[Token]) -> String {
    let mut s = String::new();
    let mut prev_line: Option<u32> = None;
    for t in toks {
        // Newlines are preserved (line directives like `#include` are line-oriented, so
        // flattening would let one swallow the rest of the block); same-line tokens are
        // space-separated.
        match prev_line {
            Some(l) if t.span.pos.line != l => s.push('\n'),
            Some(_) => s.push(' '),
            None => {}
        }
        render_kind(&t.kind, &mut s);
        prev_line = Some(t.span.pos.line);
    }
    s
}

fn render_kind(k: &TokenKind, out: &mut String) {
    use TokenKind::*;
    use std::fmt::Write;
    let lit = match k {
        Int(n) => {
            let _ = write!(out, "{n}");
            return;
        }
        Float(f) => {
            let _ = write!(out, "{f:?}"); // {:?} always keeps a `.`/`e`
            return;
        }
        Char(n) => {
            let _ = write!(out, "{n}"); // a char constant is its int value
            return;
        }
        Str(s) => return render_string(s, out),
        Ident(s) => return out.push_str(s),
        Keyword(kw) => return out.push_str(kw.as_str()),
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        Eq => "=",
        PlusEq => "+=",
        MinusEq => "-=",
        StarEq => "*=",
        SlashEq => "/=",
        PercentEq => "%=",
        AmpEq => "&=",
        PipeEq => "|=",
        CaretEq => "^=",
        ShlEq => "<<=",
        ShrEq => ">>=",
        PlusPlus => "++",
        MinusMinus => "--",
        EqEq => "==",
        Ne => "!=",
        Lt => "<",
        Gt => ">",
        Le => "<=",
        Ge => ">=",
        AndAnd => "&&",
        OrOr => "||",
        Not => "!",
        Amp => "&",
        Pipe => "|",
        Caret => "^",
        Tilde => "~",
        Shl => "<<",
        Shr => ">>",
        LParen => "(",
        RParen => ")",
        LBrace => "{",
        RBrace => "}",
        LBracket => "[",
        RBracket => "]",
        Comma => ",",
        Semicolon => ";",
        Dot => ".",
        Arrow => "->",
        Question => "?",
        Colon => ":",
        ColonColon => "::",
        ColonEq => ":=",
        DotDotDot => "...",
        At => "@",
        Hash => "#",
        Backtick => "`",
        Eof => "",
    };
    out.push_str(lit);
}

fn render_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn directive_name(tok: &Token) -> Option<String> {
    match &tok.kind {
        TokenKind::Ident(s) => Some(s.clone()),
        TokenKind::Keyword(Keyword::Else) => Some("else".to_string()),
        _ => None,
    }
}

/// The `Normal` path components of a directory, as strings (skipping the root,
/// `.`, and `..`). Used to give each source file a directory for `_`-privacy.
fn dir_components(dir: &Path) -> Vec<String> {
    dir.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The [`FileInfo`] of a file on disk — its privacy comes from its parent directory.
fn file_info_for_disk(file: &Path) -> FileInfo {
    let dir = file.parent().map(dir_components).unwrap_or_default();
    FileInfo::from_dir(dir)
}

/// The [`FileInfo`] of an embedded-stdlib file, named by its angle-include path
/// (e.g. `_impl/strhash.hc`). The embedded library is its own root namespace
/// (`<stdlib>`), so e.g. `_impl/strhash.hc` lives in dir `["<stdlib>", "_impl"]`.
fn file_info_for_embedded(angle_path: &str) -> FileInfo {
    let mut dir = vec!["<stdlib>".to_string()];
    let parts: Vec<&str> = angle_path.split('/').collect();
    for p in &parts[..parts.len().saturating_sub(1)] {
        dir.push((*p).to_string());
    }
    FileInfo::from_dir(dir)
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
    for (_i, t) in inner.iter().enumerate() {
        // Whitespace between the `<…>` tokens is tolerated (a reconstructed `#exe`
        // block may render `<math.hc>` as `< math . hc >`); the path is just the
        // concatenation of the inner tokens' text.
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
