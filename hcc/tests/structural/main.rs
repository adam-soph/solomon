//! Structural backend tests — emit each target's native image and byte-inspect its
//! container, with no execution and no toolchain, so they run on **every** host. This
//! consolidates the structural halves of the former per-backend suites
//! (`arm64_darwin`/`x86_64_linux`/`arm64_linux`/`x86_64_windows`): a green run on any one
//! host still exercises all four emitters. End-to-end *execution* parity is covered by the
//! `integration` suite (one case per `tests/cases/**/*.hc`, host-gated).
//!
//! Also home to the predefined-target-macro tests (a pure front-end concern).

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use hcc::backend::Codegen;
use hcc::sema::check_program;
use hcc::{Arm64Darwin, Arm64Linux, Program, X64Linux, X64Windows};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hcc-structural-{}-{id}", std::process::id()))
}

/// Parse + type-check `src` (embedded stdlib resolves with an empty search path).
fn checked(src: &str) -> Program {
    let program = hcc::parser::parse_with(src, Path::new("."), &[])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(
        check_program(&program).is_empty(),
        "semantic errors in: {src}"
    );
    program
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

// --- per-backend emission (pure byte production; no exec, any host) ---

/// The AArch64 Mach-O **object** (pre-`cc`-link), via `Arm64Darwin::object`.
fn macho(p: &Program) -> Vec<u8> {
    Arm64Darwin::new(temp())
        .object(p)
        .unwrap_or_else(|e| panic!("arm64 Mach-O emission failed: {e}"))
}

/// The freestanding AArch64 ELF executable, via `Arm64Linux::object`.
fn elf_arm64(p: &Program) -> Vec<u8> {
    Arm64Linux::new(temp())
        .object(p)
        .unwrap_or_else(|e| panic!("arm64 ELF emission failed: {e}"))
}

/// Emit via a backend's `run` (which writes the final image) and read the bytes back.
/// Used for the x86-64 backends, which have no in-memory `object` accessor.
fn run_to_bytes(mut backend: Box<dyn Codegen>, out: &Path, p: &Program) -> Vec<u8> {
    backend
        .run(p)
        .unwrap_or_else(|e| panic!("{} emission failed: {e}", "image"));
    let bytes = std::fs::read(out).expect("emitted image is readable");
    let _ = std::fs::remove_file(out);
    bytes
}

/// The freestanding x86-64 ELF executable.
fn elf_x64(p: &Program) -> Vec<u8> {
    let out = temp();
    run_to_bytes(Box::new(X64Linux::new(&out)), &out, p)
}

/// The self-contained x86-64 PE.
fn pe(p: &Program) -> Vec<u8> {
    let out = temp();
    run_to_bytes(Box::new(X64Windows::new(&out)), &out, p)
}

// --- container validators ---

fn assert_macho(obj: &[u8], who: &str) {
    assert_eq!(
        le_u32(obj, 0),
        0xFEED_FACF,
        "{who}: Mach-O magic (MH_MAGIC_64)"
    );
    assert_eq!(le_u32(obj, 4), 0x0100_000C, "{who}: cputype ARM64");
    assert_eq!(le_u32(obj, 12), 1, "{who}: filetype MH_OBJECT");
}

/// Validate a freestanding static ELF executable with the given `e_machine`.
fn assert_elf(elf: &[u8], machine: u16, who: &str) {
    assert_eq!(&elf[0..4], b"\x7FELF", "{who}: ELF magic");
    assert_eq!(elf[4], 2, "{who}: ELFCLASS64");
    assert_eq!(elf[5], 1, "{who}: little-endian");
    assert_eq!(le_u16(elf, 16), 2, "{who}: ET_EXEC");
    assert_eq!(le_u16(elf, 18), machine, "{who}: e_machine");
    assert_eq!(le_u16(elf, 56), 1, "{who}: one PT_LOAD (e_phnum)");
}

fn assert_pe(pe: &[u8], who: &str) {
    assert_eq!(&pe[0..2], b"MZ", "{who}: DOS magic");
    let coff = le_u32(pe, 0x3C) as usize;
    assert_eq!(&pe[coff..coff + 4], b"PE\0\0", "{who}: PE signature");
    assert_eq!(le_u16(pe, coff + 4), 0x8664, "{who}: machine AMD64");
}

/// A small but varied corpus: every backend must emit a structurally valid image for each.
/// Each program lives beside this file as `tests/structural/<name>.hc`.
const PROGRAMS: &[(&str, &str)] = &[
    ("return", include_str!("return.hc")),
    ("arith", include_str!("arith.hc")),
    ("printf", include_str!("printf.hc")),
    ("float", include_str!("float.hc")),
    ("loop", include_str!("loop.hc")),
    ("call", include_str!("call.hc")),
    ("class", include_str!("class.hc")),
    ("string", include_str!("string.hc")),
    ("global_bss", include_str!("global_bss.hc")),
    ("exceptions", include_str!("exceptions.hc")),
];

#[test]
fn every_backend_emits_valid_containers() {
    for (name, src) in PROGRAMS {
        let p = checked(src);
        assert_macho(&macho(&p), name);
        assert_elf(&elf_arm64(&p), 183, name); // EM_AARCH64
        assert_elf(&elf_x64(&p), 0x3E, name); // EM_X86_64
        assert_pe(&pe(&p), name);
    }
}

#[test]
fn macho_object_has_text_and_symbols() {
    // The Mach-O object frames a `_main` and carries its machine code.
    let obj = macho(&checked(include_str!("return.hc")));
    // LC_SEGMENT_64 (0x19) then nsects section_64s; find __text.
    let ncmds = le_u32(&obj, 16);
    let mut off = 32usize;
    let mut seg = None;
    for _ in 0..ncmds {
        if le_u32(&obj, off) == 0x19 {
            seg = Some(off);
        }
        off += le_u32(&obj, off + 4) as usize;
    }
    let seg = seg.expect("LC_SEGMENT_64");
    let nsects = le_u32(&obj, seg + 64);
    let mut found_text = false;
    let mut s = seg + 72;
    for _ in 0..nsects {
        if obj[s..s + 16].starts_with(b"__text\0") {
            let size = le_u64(&obj, s + 40);
            assert!(size > 0, "__text has machine code");
            found_text = true;
        }
        s += 80;
    }
    assert!(found_text, "Mach-O object has a __text section");
}

#[test]
fn freestanding_elf_is_self_contained() {
    // The freestanding ELFs reference no libc symbols and reserve BSS beyond the file.
    let p = checked(include_str!("self_contained.hc"));
    for (elf, who) in [(elf_arm64(&p), "arm64"), (elf_x64(&p), "x86_64")] {
        let filesz = le_u64(&elf, 96);
        let memsz = le_u64(&elf, 104);
        assert!(memsz >= filesz, "{who}: p_memsz covers the file image");
        let text = String::from_utf8_lossy(&elf);
        for libc in ["printf", "malloc", "strcpy", "strlen", "strcat"] {
            assert!(
                !text.contains(libc),
                "{who}: no libc `{libc}` in a freestanding image"
            );
        }
    }
}

#[test]
fn backend_names_are_their_triples() {
    assert_eq!(Arm64Darwin::new(temp()).name(), "aarch64-apple-darwin");
    assert_eq!(Arm64Linux::new(temp()).name(), "aarch64-unknown-linux");
    assert_eq!(X64Linux::new(temp()).name(), "x86_64-unknown-linux");
    assert_eq!(X64Windows::new(temp()).name(), "x86_64-pc-windows");
}

// --- predefined target macros (a pure front-end concern) ---

#[test]
fn target_macros_match_each_triple() {
    let win = hcc::target_macros("x86_64-pc-windows");
    for n in ["_WIN32", "_WIN64", "__x86_64__", "__HCC__"] {
        assert!(win.iter().any(|&(m, _)| m == n), "windows missing {n}");
    }
    assert!(!win.iter().any(|&(m, _)| m == "__linux__"));

    let lin = hcc::target_macros("aarch64-unknown-linux");
    for n in ["__linux__", "__unix__", "__aarch64__", "__HCC__"] {
        assert!(lin.iter().any(|&(m, _)| m == n), "linux missing {n}");
    }
    assert!(!lin.iter().any(|&(m, _)| m == "_WIN32"));

    let mac = hcc::target_macros("aarch64-apple-darwin");
    for n in [
        "__APPLE__",
        "__MACH__",
        "__unix__",
        "__aarch64__",
        "__HCC__",
    ] {
        assert!(mac.iter().any(|&(m, _)| m == n), "darwin missing {n}");
    }

    assert_eq!(
        hcc::target_macros("sparc-sun-solaris"),
        vec![("__HCC__", "1")]
    );
    assert!(win.iter().chain(&lin).chain(&mac).all(|&(_, v)| v == "1"));
}

#[test]
fn ifdef_target_macro_selects_per_target() {
    let src = include_str!("ifdef_win32.hc");
    let here = Path::new(".");
    let win = hcc::parser::parse_with_target(src, here, &[], "x86_64-pc-windows").unwrap();
    let lin = hcc::parser::parse_with_target(src, here, &[], "x86_64-unknown-linux").unwrap();
    assert_ne!(win, lin, "the two targets parse to different programs");
    assert_eq!(hcc::oracle::run_to_string(&win).unwrap(), "1\n");
    assert_eq!(hcc::oracle::run_to_string(&lin).unwrap(), "2\n");
}

#[test]
fn windows_header_is_inert_off_windows_and_parses_everywhere() {
    let src = include_str!("windows_inert.hc");
    let here = Path::new(".");
    for triple in [
        "x86_64-unknown-linux",
        "x86_64-pc-windows",
        "aarch64-apple-darwin",
    ] {
        let p = hcc::parser::parse_with_target(src, here, &[], triple)
            .unwrap_or_else(|e| panic!("parse <windows.hh> failed for {triple}: {e}"));
        assert!(check_program(&p).is_empty(), "{triple}: sema errors");
    }
}
