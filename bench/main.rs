//! HolyC-vs-C benchmark reporter.
//!
//! This is the `hcc-bench` crate's binary — split out of the main package so it never builds
//! or runs as part of `cargo test`. Its `main` runs every `bench/<name>/` pair (each holds
//! `prog.hc` + `prog.c`), compiles both with the host toolchain (HolyC via the native
//! backend, C via `cc -O2`), asserts byte-identical stdout (the only hard failure), times
//! `ITERS` runs of each, and prints a single **C-vs-HolyC table** sorted by name. Run it with
//! `cargo run -p hcc-bench --release`, or `cargo run -p hcc-bench --release -- <substring>` to
//! filter by name. A parity mismatch (or a build failure) is reported and makes the process
//! exit non-zero; timing never fails the run.
//!
//! Host-gated: skips cleanly where both binaries can't be built and executed (non-host
//! targets, or `HCC_SKIP_NATIVE`). The build/run helpers are inlined at the bottom of this
//! file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hcc::backend::Codegen;
use hcc::{Arm64Darwin, Program};

/// Iterations per side when timing. Each bench loops enough internal work that one run is
/// milliseconds, so process-spawn overhead doesn't dominate.
const ITERS: u32 = 15;

/// Flag (do not fail) a benchmark where HolyC is more than this many times slower than C.
const WARN_RATIO: f64 = 5.0;

/// One row of the report: per-run milliseconds for each side and their ratio.
struct Row {
    name: String,
    c_ms: f64,
    hc_ms: f64,
    ratio: f64,
}

fn bench_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `bench/<name>/` holding both `prog.hc` and `prog.c`, sorted by name.
fn bench_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(bench_root()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.join("prog.hc").is_file() && p.join("prog.c").is_file() {
                if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// Build both binaries, check stdout parity, and time them. `Err` on a build failure or a
/// stdout mismatch (a real failure); `Ok(row)` with timings otherwise.
fn run_bench(name: &str) -> Result<Row, String> {
    let dir = bench_root().join(name);

    // Build the HolyC program for the host target.
    let hc_src =
        std::fs::read_to_string(dir.join("prog.hc")).map_err(|e| format!("prog.hc: {e}"))?;
    let program =
        hcc::parser::parse_with(&hc_src, &dir, &[]).map_err(|e| format!("HolyC parse: {e}"))?;
    if !hcc::sema::check_program(&program).is_empty() {
        return Err("HolyC sema errors".into());
    }
    let hc_bin = temp_path(&format!("bench-{name}-hc"));
    if !build_native_to(&program, &hc_bin) {
        return Err("no host backend to build the HolyC binary".into());
    }

    // Compile the C reference with `cc -O2`.
    let c_bin = temp_path(&format!("bench-{name}-c"));
    let cc = Command::new("cc")
        .arg("-O2")
        .arg("-o")
        .arg(&c_bin)
        .arg(dir.join("prog.c"))
        .status()
        .map_err(|e| format!("cc spawn: {e}"))?;
    if !cc.success() {
        return Err("cc -O2 failed to compile prog.c".into());
    }

    // Parity: identical stdout (the only hard failure).
    let hc_out = run_capture(&hc_bin);
    let c_out = run_capture(&c_bin);
    if hc_out != c_out {
        return Err(format!(
            "stdout differs\n      holyc: {:?}\n          c: {:?}",
            String::from_utf8_lossy(&hc_out),
            String::from_utf8_lossy(&c_out),
        ));
    }

    // Timing: informational.
    let t_hc = time_runs(&hc_bin, ITERS);
    let t_c = time_runs(&c_bin, ITERS);
    let _ = std::fs::remove_file(&hc_bin);
    let _ = std::fs::remove_file(&c_bin);

    let c_ms = t_c.as_secs_f64() * 1e3 / ITERS as f64;
    let hc_ms = t_hc.as_secs_f64() * 1e3 / ITERS as f64;
    Ok(Row {
        name: name.to_string(),
        c_ms,
        hc_ms,
        ratio: hc_ms / c_ms.max(1e-9),
    })
}

/// Print the C-vs-HolyC table. Numeric columns are per-run milliseconds; `ratio` is
/// HolyC/C (lower is better), with `*` flagging anything past [`WARN_RATIO`].
fn print_table(rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    let nw = rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max("benchmark".len());

    println!();
    println!("HolyC vs C — {ITERS} runs each, host `cc -O2`, ratio = HolyC/C (lower is better)");
    println!();
    println!(
        "  {:<nw$}  {:>10}  {:>10}  {:>7}",
        "benchmark", "C (ms)", "HolyC (ms)", "ratio"
    );
    println!("  {:-<nw$}  {:->10}  {:->10}  {:->7}", "", "", "", "");
    for r in rows {
        let flag = if r.ratio > WARN_RATIO { " *" } else { "" };
        println!(
            "  {:<nw$}  {:>10.2}  {:>10.2}  {:>6.2}x{}",
            r.name, r.c_ms, r.hc_ms, r.ratio, flag,
        );
    }
    let geomean = (rows.iter().map(|r| r.ratio.ln()).sum::<f64>() / rows.len() as f64).exp();
    println!("  {:-<nw$}  {:->10}  {:->10}  {:->7}", "", "", "", "");
    println!(
        "  {:<nw$}  {:>10}  {:>10}  {:>6.2}x",
        "geomean", "", "", geomean
    );
    if rows.iter().any(|r| r.ratio > WARN_RATIO) {
        println!("\n  * more than {WARN_RATIO:.0}x slower than C");
    }
}

fn main() {
    // Optional substring filter (a positional arg after `cargo run -p hcc-bench -- <name>`;
    // flags like `--nocapture` start with `-`).
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));

    if !native_host_available() || !cc_available() {
        println!("bench: skipped (needs a runnable host backend + cc; unset HCC_SKIP_NATIVE)");
        return;
    }

    let mut names = bench_names();
    if let Some(f) = &filter {
        names.retain(|n| n.contains(f.as_str()));
    }
    if names.is_empty() {
        println!("bench: no matching benchmarks");
        return;
    }

    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for name in &names {
        match run_bench(name) {
            Ok(row) => rows.push(row),
            Err(why) => failures.push(format!("{name}: {why}")),
        }
    }

    print_table(&rows);

    if !failures.is_empty() {
        eprintln!("\n{} benchmark(s) FAILED:", failures.len());
        for f in &failures {
            eprintln!("  {f}");
        }
        std::process::exit(1);
    }
}

// ===========================================================================
// Build/run harness (formerly tests/common). Inlined so this reporter binary
// is self-contained: build a HolyC program for the host, run it, and time it.
// ===========================================================================

/// Process-wide counter so concurrently-built temp binaries never collide on a path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh, process-unique temp path with the given `tag`.
fn temp_path(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hcc-{tag}-{}-{id}", std::process::id()))
}

/// Compile `program` for the host target to `out` (HolyC → native). Panics on build failure;
/// returns `false` when this host has no runnable backend.
fn build_native_to(program: &Program, out: &Path) -> bool {
    let Some(mut backend) = host_backend(out) else {
        return false;
    };
    backend
        .run(program)
        .unwrap_or_else(|e| panic!("native build failed: {e}"));
    true
}

/// Run `bin` once with no input and return its stdout.
fn run_capture(bin: &Path) -> Vec<u8> {
    run_binary(bin, &[], &[])
}

/// Wall-clock time to run `bin` `iters` times back to back.
fn time_runs(bin: &Path, iters: u32) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        let status = Command::new(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("could not run {}: {e}", bin.display()));
        assert!(status.success(), "{} exited nonzero", bin.display());
    }
    start.elapsed()
}

/// `true` when `HCC_SKIP_NATIVE` is set: skip the (native-only) benchmarks entirely.
fn skip_native() -> bool {
    std::env::var_os("HCC_SKIP_NATIVE").is_some()
}

/// Whether `cc` is on PATH (needed to compile the C reference and link hosted Darwin).
fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether this host can build **and run** a native binary for its own target. Darwin links
/// via `cc`; the freestanding Linux ELF hosts run the emitted image directly. Other hosts
/// (Windows, Intel macOS) have no runnable benchmark backend.
fn native_host_available() -> bool {
    if skip_native() {
        return false;
    }
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        cc_available()
    } else {
        cfg!(any(
            all(target_arch = "x86_64", target_os = "linux"),
            all(target_arch = "aarch64", target_os = "linux"),
        ))
    }
}

/// The host target's code generator writing to `out`, or None if this host has no runnable
/// backend (Windows, Intel macOS, or `cc` missing on Darwin).
fn host_backend(out: &Path) -> Option<Box<dyn Codegen>> {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        if !cc_available() {
            return None;
        }
        return Some(Box::new(Arm64Darwin::new(out)));
    }
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        return Some(Box::new(hcc::X64Linux::new(out)));
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        return Some(Box::new(hcc::Arm64Linux::new(out)));
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
    )))]
    {
        let _ = out;
        None
    }
}

/// Spawn `bin` with `args` and `stdin`, returning its stdout. Stdin is fed from a thread so
/// a program that writes a lot of stdout can't deadlock against an unread stdin pipe.
fn run_binary(bin: &Path, args: &[String], stdin: &[u8]) -> Vec<u8> {
    // Retry on ETXTBSY (os error 26): a freshly-built binary may still be open for writing in
    // a sibling process, so the exec races transiently. A short bounded backoff clears it.
    let mut child = {
        let mut attempt = 0;
        loop {
            match Command::new(bin)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(26) && attempt < 50 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("could not spawn {}: {e}", bin.display()),
            }
        }
    };
    let mut sin = child.stdin.take().unwrap();
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = sin.write_all(&data);
    });
    let output = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", bin.display()));
    let _ = writer.join();
    output.stdout
}
