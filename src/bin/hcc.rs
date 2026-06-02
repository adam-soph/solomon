//! The `hcc` CLI — the HolyC **compiler**. Reads HolyC source from a file (or
//! stdin) and, by default, compiles a native binary for the host's architecture
//! and OS. A leading subcommand selects a front-end-only mode. To *run* a program
//! with the interpreter, use the matching `hci`.
//!
//! Usage:
//!   hcc [--target TRIPLE] [-o OUT] [FILE]   compile a native binary (the default)
//!   hcc check  [FILE]                       parse + semantic analysis, report errors
//!   hcc ast    [FILE]                       dump the parsed AST
//!   hcc tokens [FILE]                       dump the raw lexer token stream
//!
//! With no subcommand `hcc` builds for the host target (`-o OUT`, default
//! `a.out`); `--target TRIPLE` cross-compiles instead (`aarch64-apple-darwin` →
//! Mach-O via `cc`, `x86_64-unknown-linux` / `aarch64-unknown-linux` → a
//! freestanding static ELF, `x86_64-pc-windows` → a self-contained PE).

use std::io::Read;
use std::process::ExitCode;

use solomon::arm64::{Arm64Darwin, Arm64Linux};
use solomon::codegen::Codegen;
use solomon::x86_64::{X64Linux, X64Windows};
use solomon::{lexer, parser, sema};

/// A code-generation target: an (architecture, OS) pair, since the object format,
/// syscalls, and ABI all depend on the OS — not just the CPU.
#[derive(Clone, Copy)]
enum Target {
    Arm64Darwin,
    X64Linux,
    X64Windows,
    Arm64Linux,
}

impl Target {
    /// The target the host machine natively runs, if it is one we can emit.
    fn host() -> Option<Self> {
        if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
            Some(Target::Arm64Darwin)
        } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            Some(Target::X64Linux)
        } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
            Some(Target::Arm64Linux)
        } else {
            None
        }
    }
    /// Parse a target triple (canonical, plus a couple of common short forms). The
    /// Linux targets are **freestanding** (no libc, no linker), so the `-gnu`/
    /// `-musl` libc suffixes are deliberately not accepted — use the bare triple.
    fn from_triple(s: &str) -> Option<Self> {
        match s {
            "aarch64-apple-darwin" | "arm64-apple-darwin" | "aarch64-darwin" | "arm64-darwin" => {
                Some(Target::Arm64Darwin)
            }
            "x86_64-unknown-linux" | "x86_64-linux" => Some(Target::X64Linux),
            "x86_64-pc-windows"
            | "x86_64-pc-windows-gnu"
            | "x86_64-pc-windows-msvc"
            | "x86_64-windows" => Some(Target::X64Windows),
            "aarch64-unknown-linux" | "aarch64-linux" | "aarch64-unknown-linux-none" => {
                Some(Target::Arm64Linux)
            }
            _ => None,
        }
    }
    fn codegen(self, out: &str) -> Box<dyn Codegen> {
        match self {
            Target::Arm64Darwin => Box::new(Arm64Darwin::new(out)),
            Target::X64Linux => Box::new(X64Linux::new(out)),
            Target::X64Windows => Box::new(X64Windows::new(out)),
            Target::Arm64Linux => Box::new(Arm64Linux::new(out)),
        }
    }
}

enum Mode {
    Tokens,
    Ast,
    Check,
    Build,
}

fn main() -> ExitCode {
    // The default action is to compile a native binary for the host target.
    let mut mode = Mode::Build;
    let mut out: Option<String> = None;
    let mut target: Option<Target> = None;

    let mut args = std::env::args().skip(1).peekable();

    // An optional leading subcommand selects a non-default mode; anything else
    // (a file or an option) leaves the default build mode in place.
    let is_subcommand = match args.peek().map(String::as_str) {
        Some("build") => {
            mode = Mode::Build;
            true
        }
        Some("check") => {
            mode = Mode::Check;
            true
        }
        Some("ast") => {
            mode = Mode::Ast;
            true
        }
        Some("tokens") => {
            mode = Mode::Tokens;
            true
        }
        Some("-h" | "--help" | "help") => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        _ => false,
    };
    if is_subcommand {
        args.next();
    }

    // The single positional is the input FILE (stdin if omitted).
    let mut path: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => match args.next() {
                Some(t) => match Target::from_triple(&t) {
                    Some(tt) => target = Some(tt),
                    None => {
                        eprintln!(
                            "hcc: unknown target `{t}` (known: aarch64-apple-darwin, \
                             x86_64-unknown-linux, x86_64-pc-windows, \
                             aarch64-unknown-linux). The Linux targets are \
                             freestanding; the `-gnu`/`-musl` libc suffixes are not \
                             accepted — use the bare triple."
                        );
                        return ExitCode::FAILURE;
                    }
                },
                None => {
                    eprintln!("hcc: --target requires a triple");
                    return ExitCode::FAILURE;
                }
            },
            "-o" => match args.next() {
                Some(o) => out = Some(o),
                None => {
                    eprintln!("hcc: -o requires an output path");
                    return ExitCode::FAILURE;
                }
            },
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("hcc: unknown option `{other}`");
                return ExitCode::FAILURE;
            }
            other => path = Some(other.to_string()),
        }
    }

    let src = match &path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("hcc: cannot read `{path}`: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            // No file given: read HolyC source from stdin.
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("hcc: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    };

    // `#include "..."` paths resolve relative to the input file's directory
    // (the current directory when reading from stdin).
    let base_dir = path
        .as_ref()
        .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    match mode {
        Mode::Tokens => match lexer::tokenize(&src) {
            Ok(tokens) => {
                for tok in &tokens {
                    println!("{:>10}  {:?}", tok.span.pos.to_string(), tok.kind);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Mode::Ast => match parser::parse_in_dir(&src, &base_dir) {
            Ok(program) => {
                println!("{program:#?}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Mode::Check => match parser::parse_in_dir(&src, &base_dir) {
            Ok(program) => {
                let errors = sema::check_program(&program);
                if errors.is_empty() {
                    println!("ok: no errors");
                    ExitCode::SUCCESS
                } else {
                    for e in &errors {
                        eprintln!("{e}");
                    }
                    eprintln!("{} error(s)", errors.len());
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Mode::Build => {
            // With no `--target`, compile for the host's native target.
            let target = match target.or_else(Target::host) {
                Some(t) => t,
                None => {
                    eprintln!(
                        "hcc: this host isn't a supported native target; \
                         pass --target aarch64-apple-darwin or --target x86_64-unknown-linux"
                    );
                    return ExitCode::FAILURE;
                }
            };
            let program = match parser::parse_in_dir(&src, &base_dir) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let errors = sema::check_program(&program);
            if !errors.is_empty() {
                for e in &errors {
                    eprintln!("{e}");
                }
                eprintln!("{} error(s)", errors.len());
                return ExitCode::FAILURE;
            }
            let out_path = out.unwrap_or_else(|| "a.out".to_string());
            let mut codegen = target.codegen(&out_path);
            match codegen.run(&program) {
                Ok(()) => {
                    eprintln!("hcc: wrote {out_path}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn print_usage() {
    print!(
        "\
hcc — the HolyC compiler

Usage:
  hcc [--target TRIPLE] [-o OUT] [FILE]   compile a native binary (the default)
  hcc check  [FILE]                       parse + semantic analysis, report errors
  hcc ast    [FILE]                       dump the parsed AST
  hcc tokens [FILE]                       dump the raw lexer token stream

With no subcommand, hcc compiles for the host's architecture and OS. FILE is
read from stdin when omitted. To run a program, use `hci`.

Options:
  -o OUT            output path for the compiled binary (default: a.out)
  --target TRIPLE   cross-compile for a specific target:
                      aarch64-apple-darwin    Mach-O, linked with cc
                      x86_64-unknown-linux    freestanding static ELF
                      aarch64-unknown-linux   freestanding static ELF
                      x86_64-pc-windows       self-contained PE
  -h, --help        show this help
"
    );
}
