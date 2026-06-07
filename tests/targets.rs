//! Target registration / scaffolding tests. These are host-independent: they only
//! inspect the emitted bytes or the error, never running a foreign binary.

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::arm64::Arm64Linux;
use solomon::codegen::Codegen;
use solomon::sema::check_program;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-target-{}-{id}", std::process::id()))
}

fn checked(src: &str) -> solomon::Program {
    // Resolve any `#include <string.hc>` against the repo `lib/`. For sources without
    // angle includes this is identical to a plain parse.
    let lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");
    let program = solomon::parser::parse_with(src, std::path::Path::new("."), &[lib])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "semantic errors");
    program
}

#[test]
fn aarch64_freestanding_emits_a_static_elf_executable() {
    // The freestanding target emits a self-contained static ELF *executable* (no libc,
    // no linker), like the x86-64 Linux backend. This byte-checks its structure on any
    // host; execution is covered natively by the arm64 conformance suite. `object()`
    // returns the finished executable here, since `compile` produces the runnable image
    // directly for a freestanding target.
    let program = checked("I64 Sq(I64 x){ return x*x; } return Sq(6) + 6;");
    let elf = Arm64Linux::new(temp()).object(&program).unwrap();
    assert_eq!(&elf[0..4], b"\x7FELF", "ELF magic");
    assert_eq!(elf[4], 2, "ELFCLASS64");
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    assert_eq!(u16::from_le_bytes([elf[18], elf[19]]), 183, "EM_AARCH64");
    // One PT_LOAD, no section headers, no libc symbols: fully self-contained.
    assert_eq!(
        u16::from_le_bytes([elf[56], elf[57]]),
        1,
        "one PT_LOAD (e_phnum)"
    );
    assert_eq!(
        u16::from_le_bytes([elf[60], elf[61]]),
        0,
        "no sections (e_shnum)"
    );
    // e_entry points at the first code byte (ELF header + one program header).
    let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
    assert_eq!(
        entry,
        0x40_0000 + 64 + 56,
        "entry = _start at the image start"
    );
    assert!(
        !String::from_utf8_lossy(&elf).contains("printf"),
        "freestanding image references no libc symbols"
    );
    assert_eq!(Arm64Linux::new(temp()).name(), "aarch64-unknown-linux");
}

#[test]
fn aarch64_freestanding_prints_without_libc() {
    // A program that prints integers/strings still emits a self-contained image: no
    // `printf` (or any libc) reference. The formatting and `write` syscall are emitted
    // inline. Behaviour is verified byte-for-byte against the interpreter, natively, in
    // the arm64 conformance run. Here we just confirm the image is freestanding and
    // structurally an executable.
    let program = checked(r#"U0 Main(){ "x=%d hex=%x s=%s\n", 42, 255, "hi"; } Main;"#);
    let elf = Arm64Linux::new(temp()).object(&program).unwrap();
    assert_eq!(&elf[0..4], b"\x7FELF");
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    assert_eq!(u16::from_le_bytes([elf[18], elf[19]]), 183, "EM_AARCH64");
    let s = String::from_utf8_lossy(&elf);
    assert!(
        !s.contains("printf"),
        "no libc printf in a freestanding image"
    );
    assert!(!s.contains("sprintf"), "no libc symbols at all");
}

#[test]
fn aarch64_freestanding_globals_and_runtime_need_no_libc() {
    // Globals (BSS, self-addressed), the heap (`MAlloc` over `mmap`), and the
    // string/memory builtins are all emitted, with no libc reference and no leftover
    // relocation. Behaviour is checked against the interpreter by the per-target
    // conformance suites.
    let program = checked(
        r#"#include <string.hc>
           I64 g = 5; U0 Main(){ U8 *b = MAlloc(32); StrCpy(b, "hi"); StrCat(b, "!");
           "%s %d %d\n", b, StrLen(b), g; } Main;"#,
    );
    let elf = Arm64Linux::new(temp()).object(&program).unwrap();
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    // BSS is reserved beyond the file image (globals and allocator state).
    let filesz = u64::from_le_bytes(elf[96..104].try_into().unwrap());
    let memsz = u64::from_le_bytes(elf[104..112].try_into().unwrap());
    assert!(memsz > filesz, "p_memsz reserves BSS beyond the file image");
    let s = String::from_utf8_lossy(&elf);
    for libc in ["printf", "malloc", "strcpy", "strlen", "strcat"] {
        assert!(
            !s.contains(libc),
            "no libc `{libc}` in a freestanding image"
        );
    }
}

#[test]
fn aarch64_freestanding_float_printf_needs_no_libc() {
    // `%f` is emitted as the inline bignum formatter (no libc `printf`/`snprintf`),
    // correctly rounded to match the interpreter. Behaviour (incl. round-half-even) is
    // checked against the interpreter by the conformance suites. Here we confirm the
    // image is freestanding.
    let program = checked(r#"U0 Main(){ "%.2f %9.3f %f\n", 2.5, 3.14159, -0.001; } Main;"#);
    let elf = Arm64Linux::new(temp()).object(&program).unwrap();
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    let s = String::from_utf8_lossy(&elf);
    for libc in ["printf", "snprintf", "sprintf"] {
        assert!(
            !s.contains(libc),
            "no libc `{libc}` for freestanding float printing"
        );
    }
}
