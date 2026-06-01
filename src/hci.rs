//! The `hci` CLI — the HolyC interpreter.
//!
//! Runs a HolyC program with the tree-walking interpreter (the conformance
//! oracle for the native backends). Reads from FILE, or from stdin if none is
//! given; arguments after FILE become the program's own `argv` (readable via
//! `ArgC`/`ArgV`). To compile a HolyC program to a native binary instead, use the
//! matching ahead-of-time compiler, `hcc`.
//!
//! Usage: hci [FILE] [ARGS...]      (`--` ends option parsing; `-h` for help)

use std::io::Read;
use std::process::ExitCode;

use solomon::interp::Interpreter;
use solomon::{parser, sema};

fn main() -> ExitCode {
    // Positionals: FILE (or none = stdin), then the program's own arguments. The
    // first positional is the file; everything after it is a program argument
    // (even if it looks like an option), and `--` forces that early.
    let mut positionals: Vec<String> = Vec::new();
    let mut opts_done = false;
    for arg in std::env::args().skip(1) {
        if opts_done {
            positionals.push(arg);
        } else if arg == "--" {
            opts_done = true;
        } else if matches!(arg.as_str(), "-h" | "--help" | "help") {
            print_usage();
            return ExitCode::SUCCESS;
        } else if arg.starts_with('-') {
            eprintln!("hci: unknown option `{arg}`");
            return ExitCode::FAILURE;
        } else {
            positionals.push(arg);
            opts_done = true; // the file is set; the rest are the program's args
        }
    }
    let path = positionals.first().cloned();

    let src = match &path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("hci: cannot read `{path}`: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("hci: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    };

    // `#include "..."` resolves relative to the file's directory (CWD for stdin).
    let base_dir = path
        .as_ref()
        .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

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
    // argv[0] is the script name (or "hci" for stdin); the rest are the trailing
    // command-line arguments.
    interp.set_args(if positionals.is_empty() {
        vec!["hci".to_string()]
    } else {
        positionals
    });
    match interp.run(&program) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    print!(
        "\
hci — the HolyC interpreter

Usage:
  hci [FILE] [ARGS...]   run a HolyC program (FILE, or stdin when omitted)

Arguments after FILE become the program's argv (read with ArgC/ArgV); `--` ends
option parsing so option-looking program arguments pass through. To compile a
program to a native binary instead, use `hcc`.

  -h, --help   show this help
"
    );
}
