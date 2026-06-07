//! Tests for the x86-64 / PE (Windows) backend.
//!
//! The backend can't be executed on the dev host. An Apple-silicon Mac runs the
//! amd64 image under QEMU, where Wine can't run x86 Windows code. So these tests
//! byte-scan the emitted PE instead, like the x86-64 Linux structural tests do.
//! They check the PE32+ headers and the exact instruction bytes of each `kernel32`
//! shim, confirming that every `call [rip]` resolves through the import address
//! table to the function it should. This pins the generated code without running it.

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;
use solomon::x86_64::X64Windows;

mod common;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_out() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-win-{}-{id}.exe", std::process::id()))
}

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

/// Parse + sema `src` (with the stdlib search path), returning the program.
fn compile(src: &str) -> solomon::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    program
}

/// Compile `src` to a PE image (written to a temp file, then read back).
fn build_pe(src: &str) -> Vec<u8> {
    let out = temp_out();
    X64Windows::new(&out)
        .run(&compile(src))
        .unwrap_or_else(|e| panic!("windows build failed: {e}"));
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    bytes
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_i32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The single section's `(file offset, virtual address, raw size)`.
fn section(pe: &[u8]) -> (usize, u32, usize) {
    let sect = le_u32(pe, 0x3c) as usize + 24 + 240; // PE sig + COFF(24) + optional header(240)
    let vaddr = le_u32(pe, sect + 12);
    let raw_size = le_u32(pe, sect + 16) as usize;
    let praw = le_u32(pe, sect + 20) as usize;
    (praw, vaddr, raw_size)
}

/// Map an RVA to a file offset through the single section.
fn rva_to_off(pe: &[u8], rva: u32) -> usize {
    let (praw, vaddr, _) = section(pe);
    (rva - vaddr) as usize + praw
}

/// File offset of the first occurrence of `needle` in the code section.
fn find_code(pe: &[u8], needle: &[u8]) -> usize {
    let (praw, _, raw) = section(pe);
    let code = &pe[praw..praw + raw];
    praw + code
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("byte pattern {needle:02X?} not found in code"))
}

/// The `kernel32` function name that a `call qword [rip+disp32]` at file offset
/// `pos` resolves to. Follow the RIP-relative displacement to an IAT slot, then
/// follow the slot's RVA to its hint/name entry.
fn call_target(pe: &[u8], pos: usize) -> String {
    assert_eq!(
        &pe[pos..pos + 2],
        &[0xFF, 0x15],
        "expected `call [rip]` (FF 15) at {pos:#x}"
    );
    let (praw, vaddr, _) = section(pe);
    let next_rva = vaddr as i64 + (pos - praw) as i64 + 6; // RIP after the 6-byte call
    let slot_rva = (next_rva + le_i32(pe, pos + 2) as i64) as u32; // IAT slot
    let name_rva = le_u64(pe, rva_to_off(pe, slot_rva)) as u32; // -> hint/name entry
    let hn = rva_to_off(pe, name_rva) + 2; // skip the 2-byte hint
    let len = pe[hn..].iter().position(|&b| b == 0).unwrap();
    String::from_utf8_lossy(&pe[hn..hn + len]).into_owned()
}

#[test]
fn produces_a_valid_pe32plus_executable() {
    let pe = build_pe("return 42;");

    // DOS header: "MZ" and e_lfanew pointing at the PE signature.
    assert_eq!(&pe[0..2], b"MZ", "DOS magic");
    let coff = le_u32(&pe, 0x3c) as usize;
    assert_eq!(coff, 0x40, "e_lfanew");
    assert_eq!(&pe[coff..coff + 4], b"PE\0\0", "PE signature");

    // COFF header.
    assert_eq!(le_u16(&pe, coff + 4), 0x8664, "machine should be AMD64");
    assert_eq!(le_u16(&pe, coff + 6), 1, "one section");
    assert_eq!(le_u16(&pe, coff + 20), 240, "SizeOfOptionalHeader");

    // Optional header (PE32+).
    let opt = coff + 24;
    assert_eq!(le_u16(&pe, opt), 0x020B, "PE32+ magic");
    assert_eq!(
        le_u32(&pe, opt + 16),
        0x1000,
        "entry point at the section start"
    );
    assert_eq!(le_u64(&pe, opt + 24), 0x1_4000_0000, "ImageBase");
    assert_eq!(le_u16(&pe, opt + 68), 3, "Subsystem = console");

    // The one section is R+W+X and maps the image.
    let sect = opt + 240;
    assert_eq!(&pe[sect..sect + 5], b".text", "section name");
    assert_eq!(le_u32(&pe, sect + 12), 0x1000, "section VirtualAddress");
    assert_eq!(
        le_u32(&pe, sect + 36),
        0xE000_0020,
        "section is CODE|EXEC|READ|WRITE"
    );
}

#[test]
fn entry_is_framed_and_exits_via_exitprocess() {
    let pe = build_pe("return 42;");
    let (praw, _, _) = section(&pe);

    // The entry opens a frame (`push rbp; mov rbp, rsp`).
    assert_eq!(
        &pe[praw..praw + 4],
        &[0x55, 0x48, 0x89, 0xE5],
        "frame prologue"
    );

    // The exit shim: `mov ecx, eax; sub rsp, 32; call [rip]` -> ExitProcess.
    let shim = [0x89, 0xC1, 0x48, 0x83, 0xEC, 0x20, 0xFF, 0x15];
    let at = find_code(&pe, &shim);
    assert_eq!(call_target(&pe, at + 6), "ExitProcess");
}

#[test]
fn printing_lowers_to_getstdhandle_then_writefile() {
    let pe = build_pe("\"hi\\n\";");

    // A bare string lowers to `StdWrite(STDOUT, …)`. Its Windows shim picks the std
    // handle from the fd before calling `GetStdHandle`. Match the shim byte for byte
    // (the disp32s aside):
    //   sub rsp,72; mov [rsp+48],rsi; mov [rsp+56],rdx;
    //   mov ecx,-11; cmp rdi,2; jne +5; mov ecx,-12; call [GetStdHandle]
    let head = [
        0x48, 0x83, 0xEC, 0x48, // sub rsp, 72
        0x48, 0x89, 0x74, 0x24, 0x30, // mov [rsp+48], rsi
        0x48, 0x89, 0x54, 0x24, 0x38, // mov [rsp+56], rdx
        0xB9, 0xF5, 0xFF, 0xFF, 0xFF, // mov ecx, -11 (STD_OUTPUT_HANDLE)
        0x48, 0x83, 0xFF, 0x02, // cmp rdi, 2
        0x75, 0x05, // jne +5 (keep stdout)
        0xB9, 0xF4, 0xFF, 0xFF, 0xFF, // mov ecx, -12 (STD_ERROR_HANDLE)
        0xFF, 0x15, // call [rip] (GetStdHandle)
    ];
    let start = find_code(&pe, &head);
    let gsh = start + head.len() - 2; // the FF 15 of the first call
    assert_eq!(call_target(&pe, gsh), "GetStdHandle");

    // Then marshal the WriteFile args and call it.
    let mid = [
        0x48, 0x89, 0xC1, // mov rcx, rax (handle)
        0x48, 0x8B, 0x54, 0x24, 0x30, // mov rdx, [rsp+48] (buf)
        0x4C, 0x8B, 0x44, 0x24, 0x38, // mov r8, [rsp+56] (len)
        0x4C, 0x8D, 0x4C, 0x24, 0x28, // lea r9, [rsp+40] (&written)
        0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00, // mov qword [rsp+32], 0
        0xFF, 0x15, // call [rip] (WriteFile)
    ];
    let mid_at = gsh + 6; // just past the GetStdHandle call
    assert_eq!(
        &pe[mid_at..mid_at + mid.len()],
        &mid,
        "WriteFile marshaling"
    );
    let wf = mid_at + mid.len() - 2;
    assert_eq!(call_target(&pe, wf), "WriteFile");
    // ... return the written count, then tear the frame down.
    assert_eq!(
        &pe[wf + 6..wf + 10],
        &[0x8B, 0x44, 0x24, 0x28],
        "mov eax, [rsp+40] (written → rax)"
    );
    assert_eq!(
        &pe[wf + 10..wf + 14],
        &[0x48, 0x83, 0xC4, 0x48],
        "add rsp, 72"
    );
}

#[test]
fn args_capture_calls_getcommandline() {
    // A program using ArgC/ArgV captures the command line at the entry. So the first
    // `call [rip]` after the frame prologue is `GetCommandLineA`.
    let pe = build_pe("\"%d\\n\", ArgC;");
    // The capture opens with `sub rsp, 32; call [rip]` (shadow space + the call).
    let at = find_code(&pe, &[0x48, 0x83, 0xEC, 0x20, 0xFF, 0x15]);
    assert_eq!(call_target(&pe, at + 4), "GetCommandLineA");
}

#[test]
fn malloc_lowers_to_virtualalloc() {
    let pe = build_pe("#include <mem.hc>\nU8 *p = MAlloc(16); p[0] = 7;");

    // The `emit_page_alloc` shim, byte for byte (disp32 aside):
    //   xor ecx,ecx; mov rdx,rsi; mov r8d,0x3000; mov r9d,4; sub rsp,32; call [VirtualAlloc]; add rsp,32
    let shim = [
        0x31, 0xC9, // xor ecx, ecx
        0x48, 0x89, 0xF2, // mov rdx, rsi
        0x41, 0xB8, 0x00, 0x30, 0x00, 0x00, // mov r8d, 0x3000
        0x41, 0xB9, 0x04, 0x00, 0x00, 0x00, // mov r9d, 4
        0x48, 0x83, 0xEC, 0x20, // sub rsp, 32
        0xFF, 0x15, // call [rip] (VirtualAlloc)
    ];
    let at = find_code(&pe, &shim);
    let call = at + shim.len() - 2;
    assert_eq!(call_target(&pe, call), "VirtualAlloc");
    assert_eq!(
        &pe[call + 6..call + 6 + 4],
        &[0x48, 0x83, 0xC4, 0x20],
        "add rsp, 32"
    );
}

#[test]
fn time_builtins_lower_to_kernel32() {
    // Each time builtin marshals its args, then calls a kernel32 import behind the
    // `call_aligned` shim (save rsp in r15, 16-align, 32-byte shadow, call [rip]).
    // The PE can't run here, so byte-scan for the shim and resolve the import.
    let shim: &[u8] = &[
        0x49, 0x89, 0xE7, // mov r15, rsp
        0x48, 0x81, 0xE4, 0xF0, 0xFF, 0xFF, 0xFF, // and rsp, -16
        0x48, 0x83, 0xEC, 0x20, // sub rsp, 32 (shadow)
        0xFF, 0x15, // call [rip] (the import)
    ];
    for (src, fname) in [
        ("I64 t = UnixNS();", "GetSystemTimePreciseAsFileTime"),
        ("I64 t = NanoNS();", "GetTickCount64"),
        ("Sleep(1000000);", "Sleep"),
    ] {
        let pe = build_pe(&format!("#include <time.hc>\n{src}"));
        let at = find_code(&pe, shim);
        let call = at + shim.len() - 2;
        assert_eq!(call_target(&pe, call), fname, "for `{src}`");
    }
}

#[test]
fn file_io_lowers_to_kernel32() {
    // `Open`/`Write`/`Close` lower to `CreateFileA`/`WriteFile`/`CloseHandle`. A
    // print-free program keeps the `WriteFile`/`CloseHandle` calls unambiguous, with
    // no `StdWrite` from a `Print`. The shims force-16-align rsp before the call, so
    // scan the alignment-independent arg-marshalling tails just before each
    // `call [rip]`.
    let pe = build_pe(
        "#include <io.hc>\n\
         U0 Main() { I64 fd = Open(\"t\", O_WRONLY|O_CREAT|O_TRUNC, MODE_0644);\n\
         U8 *m = \"hi\"; Write(fd, m, 2); Close(fd); }\n\
         Main;",
    );
    // CreateFileA: the two trailing stack-arg stores (FILE_ATTRIBUTE_NORMAL = 0x80,
    // hTemplateFile = NULL) immediately precede the `call [rip]`.
    let cf = find_code(
        &pe,
        &[
            0x48, 0xC7, 0x44, 0x24, 0x28, 0x80, 0x00, 0x00, 0x00, // mov qword [rsp+40], 0x80
            0x48, 0xC7, 0x44, 0x24, 0x30, 0x00, 0x00, 0x00, 0x00, // mov qword [rsp+48], 0
            0xFF, 0x15, // call [rip]
        ],
    );
    assert_eq!(call_target(&pe, cf + 18), "CreateFileA");
    // WriteFile: the `lpOverlapped = NULL` store then the call (the Read/Write tail).
    let wf = find_code(
        &pe,
        &[
            0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00, // mov qword [rsp+32], 0
            0xFF, 0x15, // call [rip]
        ],
    );
    assert_eq!(call_target(&pe, wf + 9), "WriteFile");
}

#[test]
fn posix_only_builtins_rejected_on_windows() {
    // The syscall-group primitives with no Windows lowering yet (sockets, filesystem
    // mutation, process ids, working dir, threads, futex) must be a clear compile
    // error on the PE target, not a silently-emitted, invalid Linux `syscall`. File
    // I/O is wired via kernel32, so it is not in this set.
    for (src, name) in [
        (
            "#include <os.hc>\nU0 Main(){ Mkdir(\"d\", 0700); } Main;",
            "Mkdir",
        ),
        (
            "#include <os.hc>\nU0 Main(){ Remove(\"f\"); } Main;",
            "Remove",
        ),
        (
            "#include <os.hc>\nU0 Main(){ I64 p = Getpid(); } Main;",
            "Getpid",
        ),
    ] {
        let program = parse_with(src, std::path::Path::new("."), &[lib_dir()]).unwrap();
        assert!(check_program(&program).is_empty(), "{name}: sema errors");
        let out = temp_out();
        let err = X64Windows::new(&out).run(&program).unwrap_err();
        let _ = std::fs::remove_file(&out);
        assert!(
            err.to_string()
                .contains("not supported on the Windows target"),
            "{name}: expected a clear Windows-unsupported error, got: {err}"
        );
    }
}

// ---- execution conformance: the emitted PE matches the interpreter ----
//
// The structural checks above pin the generated bytes. These tests instead compile
// each program to a PE, run it, and assert its stdout equals the interpreter's (the
// conformance oracle). It's the same byte-for-byte check the other three backends get.
// Building the PE happens on every host, so the compilation is exercised everywhere.
// The program is only executed on a native Windows host; the comparison self-skips
// elsewhere, since a non-Windows runner can't run a PE.

/// Run each PE at `paths` and capture its stdout, natively and only on a Windows
/// host. Returns `None` to skip off Windows.
fn run_stdouts(paths: &[std::path::PathBuf]) -> Option<Vec<String>> {
    use std::process::Command;
    if !cfg!(target_os = "windows") {
        return None;
    }
    paths
        .iter()
        .map(|p| {
            Command::new(p)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .collect()
}

/// For each `(name, program)`, capture the interpreter's stdout and build the PE. On a
/// Windows host, also run the PE and assert its stdout matches byte-for-byte.
fn run_pe_conformance(cases: Vec<(String, solomon::Program)>) {
    let mut paths = Vec::new();
    let mut wants = Vec::new();
    for (name, program) in &cases {
        let want = run_to_string(program).unwrap_or_else(|e| panic!("{name}: interp error: {e}"));
        let out = temp_out();
        X64Windows::new(&out)
            .run(program)
            .unwrap_or_else(|e| panic!("{name}: windows build failed: {e}"));
        paths.push(out);
        wants.push((name.clone(), want));
    }
    let got = run_stdouts(&paths);
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let Some(got) = got else {
        eprintln!("skipping PE execution conformance: needs a windows host");
        return;
    };
    for ((name, want), out) in wants.iter().zip(got) {
        assert_eq!(&out, want, "PE stdout != interpreter for `{name}`");
    }
}

#[test]
fn pe_examples_match_the_interpreter() {
    // The whole shared example set (`common::EXAMPLES`) compiles to a PE and prints
    // exactly what the interpreter does. It's the same catch-all the arm64 and
    // freestanding backends enforce, now executed natively on Windows.
    let cases = common::EXAMPLES
        .iter()
        .map(|(name, src)| {
            let program =
                common::parse_example(src).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
            assert!(
                check_program(&program).is_empty(),
                "{name}: semantic errors"
            );
            (name.to_string(), program)
        })
        .collect();
    run_pe_conformance(cases);
}

#[test]
fn pe_printing_matches_the_interpreter() {
    // A few targeted format edge cases beyond what the examples cover (signedness on
    // `>>`/`/`/`%`, `*` width/precision, `#` flags).
    let cases: &[(&str, &str)] = &[
        (
            "width",
            r#"U0 Main(){ "[%5d][%-5d][%05d][%+d][%#x][%#o]\n", 42, 42, 42, 7, 255, 64; } Main;"#,
        ),
        (
            "starwp",
            r#"U0 Main(){ "[%*d][%.*d][%-10s][%.3s]\n", 6, 42, 4, 42, "hi", "hello"; } Main;"#,
        ),
        (
            "signed",
            r#"U0 Main(){ I64 a=-8; U64 b=0x8000000000000000; "%d %x %d %d\n", a>>1, b>>4, a/2, a%3; } Main;"#,
        ),
        (
            "floatfmt",
            r#"U0 Main(){ "%.3f %.2e %g %g\n", 7.0, 12345.678, 0.000123, 42.5; } Main;"#,
        ),
    ];
    run_pe_conformance(
        cases
            .iter()
            .map(|(n, s)| (n.to_string(), compile(s)))
            .collect(),
    );
}

#[test]
fn pe_env_matches_interpreter() {
    // `EnvP`/`Getenv` work on Windows: the entry builds the `U8 **EnvP` array over
    // `GetEnvironmentStringsA` (skipping the leading-`=` per-drive entries to match the
    // interpreter's `std::env` view). Output is presence-based so it's deterministic and
    // matches the interpreter on the same host. Runs only on Windows; self-skips else.
    run_pe_conformance(vec![(
        "env".to_string(),
        compile(
            "#include <os.hc>\n\
             U0 Main(){ I64 n = 0; while (EnvP[n]) n++; \
               \"%d %d %d\\n\", n > 0, Getenv(\"PATH\") != NULL, Getenv(\"NOPE_X9Z7\") != NULL; } \
             Main;",
        ),
    )]);
}

#[test]
fn pe_file_io_matches_interpreter() {
    // File I/O end-to-end: write a string to a temp file via the kernel32
    // `CreateFileA`/`WriteFile`, read it back via `ReadFile`, and print it. The
    // interpreter (over `std::fs`) and the executed PE must agree byte-for-byte. This
    // runs only on a Windows host and self-skips elsewhere. A process-unique temp path
    // keeps the repo clean: the interpreter's write runs on every host while computing
    // the expected output. Backslashes are normalized to forward slashes so the path
    // embeds cleanly in a HolyC string literal; Windows file APIs accept them.
    let path = std::env::temp_dir()
        .join(format!("solomon-pe-io-{}.txt", std::process::id()))
        .to_string_lossy()
        .replace('\\', "/");
    let _ = std::fs::remove_file(&path);
    let src = format!(
        "#include <io.hc>\n\
        U0 Main() {{\n\
          U8 *m = \"solomon\\n\";\n\
          WriteFile(\"{path}\", m, StrLen(m));\n\
          U8 buf[64];\n\
          I64 n = ReadFile(\"{path}\", buf, 64);\n\
          if (n < 0) {{ \"read failed\\n\"; return; }}\n\
          buf[n] = 0;\n\
          \"got: %s\", buf;\n\
        }}\n\
        Main;"
    );
    run_pe_conformance(vec![("file_io".to_string(), compile(&src))]);
    let _ = std::fs::remove_file(&path);
}
