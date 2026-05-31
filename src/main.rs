//! Solomon CLI. Reads HolyC source from a file (or stdin) and either dumps the
//! token stream or the parsed AST — useful for eyeballing the front end.
//!
//! Usage:
//!   solomon [--tokens|--ast|--check|--run|--build] [-o OUT] [FILE]
//!
//! `--tokens` runs the lexer only; `--ast` runs the parser; `--check` runs the
//! parser plus semantic analysis; `--run` (the default) checks then executes the
//! program with the tree-walking interpreter; `--build` compiles to a native
//! executable (written to OUT, default `a.out`) via the AArch64 backend.

use std::io::Read;
use std::process::ExitCode;

use solomon::backend::Backend;
use solomon::backend::arm64::Arm64;
use solomon::backend::interp::Interpreter;
use solomon::{lexer, parser, sema};

enum Mode {
    Tokens,
    Ast,
    Check,
    Run,
    Build,
}

fn main() -> ExitCode {
    let mut mode = Mode::Run;
    let mut path: Option<String> = None;
    let mut out: Option<String> = None;

    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tokens" => mode = Mode::Tokens,
            "--ast" => mode = Mode::Ast,
            "--check" => mode = Mode::Check,
            "--run" => mode = Mode::Run,
            "--build" => mode = Mode::Build,
            "-o" => match args.next() {
                Some(o) => out = Some(o),
                None => {
                    eprintln!("solomon: -o requires an output path");
                    return ExitCode::FAILURE;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("solomon: unknown option `{other}`");
                return ExitCode::FAILURE;
            }
            other => path = Some(other.to_string()),
        }
    }

    let src = match &path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("solomon: cannot read `{path}`: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            // No file given: read HolyC source from stdin.
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("solomon: cannot read stdin: {e}");
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
        Mode::Run => {
            let program = match parser::parse_in_dir(&src, &base_dir) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            // Refuse to run a program that fails semantic analysis.
            let errors = sema::check_program(&program);
            if !errors.is_empty() {
                for e in &errors {
                    eprintln!("{e}");
                }
                eprintln!("{} error(s)", errors.len());
                return ExitCode::FAILURE;
            }
            let stdout = std::io::stdout();
            let mut interp = Interpreter::new(stdout.lock());
            match interp.run(&program) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Mode::Build => {
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
            let mut backend = Arm64::new(&out_path);
            match backend.run(&program) {
                Ok(()) => {
                    eprintln!("solomon: wrote {out_path}");
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
