//! Socket / networking intrinsic tests (`lib/net.hc`).
//!
//! Networking is impure, so these are **property** tests: a HolyC program connects
//! to a local echo server, sends a few bytes, and reads back what the server returns.
//! Against a deterministic echo server the interpreter and the native backends still
//! produce the same output, so they double as a conformance check.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;
use solomon::{Arm64Darwin, Arm64Linux, X64Linux};

/// Bind a one-shot TCP echo server on 127.0.0.1 (OS-assigned port) and return the
/// port. The listener is already listening before we return (so a connecting client
/// never races the `accept`); a thread accepts one connection, replies with
/// `"echo:" + <received bytes>`, and closes.
fn spawn_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 256];
            if let Ok(n) = sock.read(&mut buf) {
                let mut out = b"echo:".to_vec();
                out.extend_from_slice(&buf[..n]);
                let _ = sock.write_all(&out);
            }
        }
    });
    port
}

/// A HolyC program that connects to `127.0.0.1:port`, sends `"ping"`, and prints the
/// reply.
fn echo_program(port: u16) -> String {
    format!(
        r#"
        #include <net.hc>
        U0 Main() {{
          I64 fd = TcpConnect(ParseIPv4("127.0.0.1"), {port});
          if (fd < 0) {{ "connect: %s\n", StrError(-fd); return; }}
          Write(fd, "ping", 4);
          U8 buf[64];
          I64 n = Read(fd, buf, 64);
          if (n > 0) buf[n] = 0; else buf[0] = 0;
          "received: %s\n", buf;
          Close(fd);
        }}
        Main;
    "#
    )
}

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

#[test]
fn interp_tcp_echo_roundtrip() {
    let port = spawn_echo();
    let src = echo_program(port);
    let program = parse_with(&src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    let out = run_to_string(&program).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, "received: echo:ping\n");
}

/// Whether this host can build *and execute* an arm64 Mach-O binary.
fn darwin_toolchain() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "macos"))
        && Command::new("cc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// The same echo round-trip, but through the **native arm64 Darwin** backend: the
/// socket primitives lower to libc `socket`/`connect`/`read`/`write`/`close`. Asserts
/// the compiled binary's stdout matches the interpreter's. Self-skips off an
/// Apple-silicon host (the freestanding socket syscalls are exercised separately).
#[test]
fn native_arm64_tcp_echo_roundtrip() {
    if !darwin_toolchain() {
        eprintln!("skipping: native socket test needs aarch64-apple-darwin + cc");
        return;
    }
    let port = spawn_echo();
    let src = echo_program(port);
    let program = parse_with(&src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");

    let out = std::env::temp_dir().join(format!("solomon-net-{}", std::process::id()));
    Arm64Darwin::new(&out)
        .run(&program)
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&out);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "received: echo:ping\n"
    );
}

/// Build `src` with `backend` to a temp ELF and run it **natively** (only on a
/// matching Linux host) against the in-process host echo server. The freestanding
/// socket *syscalls* (socket/connect/write/read/close) hit the real kernel. Returns
/// the program's stdout.
fn freestanding_socket_stdout(
    out: &std::path::Path,
    mut backend: impl Codegen,
    src: &str,
) -> String {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    backend
        .run(&program)
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The connect/send/recv round-trip through the **freestanding x86-64** backend —
/// raw Linux socket syscalls (no libc). Runs only on a linux/x86_64 host (CI);
/// self-skips elsewhere.
#[test]
fn native_x86_64_freestanding_tcp_echo() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 socket test needs a linux/x86_64 host");
        return;
    }
    let port = spawn_echo();
    let out = std::env::temp_dir().join(format!("solomon-x64-net-{}", std::process::id()));
    let got = freestanding_socket_stdout(&out, X64Linux::new(&out), &echo_program(port));
    assert_eq!(got, "received: echo:ping\n", "x86_64 freestanding");
}

/// The same round-trip through the **freestanding aarch64** backend (raw arm64 Linux
/// socket syscalls). Runs only on a linux/aarch64 host; self-skips elsewhere.
#[test]
fn native_arm64_freestanding_tcp_echo() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 socket test needs a linux/aarch64 host");
        return;
    }
    let port = spawn_echo();
    let out = std::env::temp_dir().join(format!("solomon-arm-net-{}", std::process::id()));
    let got = freestanding_socket_stdout(&out, Arm64Linux::new(&out), &echo_program(port));
    assert_eq!(got, "received: echo:ping\n", "arm64 freestanding");
}
