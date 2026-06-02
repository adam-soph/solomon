//! Target registration / scaffolding tests (host-independent — they only inspect
//! the emitted bytes or the error, never run a foreign binary).

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::arm64::Arm64Linux;
use solomon::codegen::Codegen;
use solomon::parser::parse;
use solomon::sema::check_program;
use solomon::x86_64::X64Linux;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("solomon-target-{}-{id}", std::process::id()))
}

fn checked(src: &str) -> solomon::Program {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "semantic errors");
    program
}

#[test]
fn musl_is_byte_identical_to_the_gnu_static_elf() {
    // The x86-64 Linux backend emits a freestanding static ELF (no libc), so the
    // `-gnu` and `-musl` targets produce identical binaries; `-musl` only reports
    // a different triple.
    let program = checked("return 7;");
    let (gnu, musl) = (temp(), temp());
    X64Linux::new(&gnu).run(&program).unwrap();
    X64Linux::with_triple(&musl, "x86_64-unknown-linux-musl")
        .run(&program)
        .unwrap();
    let (a, b) = (std::fs::read(&gnu).unwrap(), std::fs::read(&musl).unwrap());
    let _ = (std::fs::remove_file(&gnu), std::fs::remove_file(&musl));
    assert_eq!(a, b, "musl and gnu must produce identical static ELFs");

    assert_eq!(
        X64Linux::with_triple("unused", "x86_64-unknown-linux-musl").name(),
        "x86_64-unknown-linux-musl"
    );
}

#[test]
fn aarch64_linux_emits_a_valid_elf_object() {
    // The backend emits an AArch64 ELF relocatable object (linked with gcc — not
    // exercised here). Byte-check the object structure on any host.
    let program = checked(r#""%d\n", 42;"#);
    let obj = Arm64Linux::new(temp()).object(&program).unwrap();
    assert_eq!(&obj[0..4], b"\x7FELF", "ELF magic");
    assert_eq!(obj[4], 2, "ELFCLASS64");
    assert_eq!(u16::from_le_bytes([obj[16], obj[17]]), 1, "ET_REL");
    assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 183, "EM_AARCH64");
    // Bare ELF symbol names (no Mach-O leading underscore): `main` and `printf`.
    let s = String::from_utf8_lossy(&obj);
    assert!(
        s.contains("main") && s.contains("printf"),
        "symbols present"
    );
    assert!(
        !s.contains("_main"),
        "ELF symbols are not underscore-prefixed"
    );
}

#[test]
fn aarch64_freestanding_emits_a_static_elf_executable() {
    // The freestanding target emits a self-contained static ELF *executable* (no
    // libc, no linker) — like the x86-64 Linux backend. Byte-check its structure
    // on any host; `programs_run_under_docker`-style execution is covered by the
    // arm64 conformance suite. `object()` returns the finished executable here,
    // since `compile` produces the runnable image directly for a freestanding
    // target.
    let program = checked("I64 Sq(I64 x){ return x*x; } return Sq(6) + 6;");
    let elf = Arm64Linux::new_freestanding(temp())
        .object(&program)
        .unwrap();
    assert_eq!(&elf[0..4], b"\x7FELF", "ELF magic");
    assert_eq!(elf[4], 2, "ELFCLASS64");
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    assert_eq!(u16::from_le_bytes([elf[18], elf[19]]), 183, "EM_AARCH64");
    // One PT_LOAD, no section headers, no libc symbols — fully self-contained.
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
    assert_eq!(
        Arm64Linux::new_freestanding(temp()).name(),
        "aarch64-unknown-linux"
    );
}

#[test]
fn aarch64_freestanding_prints_without_libc() {
    // A program that prints integers/strings still emits a self-contained image —
    // no `printf` (or any libc) reference; the formatting + `write` syscall are
    // emitted inline. Behaviour is verified byte-for-byte against the interpreter
    // under docker in the arm64 conformance run; here we just confirm the image is
    // freestanding and structurally an executable.
    let program = checked(r#"U0 Main(){ "x=%d hex=%x s=%s\n", 42, 255, "hi"; } Main;"#);
    let elf = Arm64Linux::new_freestanding(temp())
        .object(&program)
        .unwrap();
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
    // string/memory builtins are all emitted — no libc reference and no leftover
    // relocation. Behaviour is checked against the interpreter under docker.
    let program = checked(
        r#"I64 g = 5; U0 Main(){ U8 *b = MAlloc(32); StrCpy(b, "hi"); StrCat(b, "!");
           "%s %d %d\n", b, StrLen(b), g; } Main;"#,
    );
    let elf = Arm64Linux::new_freestanding(temp())
        .object(&program)
        .unwrap();
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    // BSS is reserved beyond the file image (globals + allocator state).
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
    // correctly rounded to match the interpreter. Behaviour (incl. round-half-even)
    // is checked against the interpreter under docker; here we confirm the image is
    // freestanding.
    let program = checked(r#"U0 Main(){ "%.2f %9.3f %f\n", 2.5, 3.14159, -0.001; } Main;"#);
    let elf = Arm64Linux::new_freestanding(temp())
        .object(&program)
        .unwrap();
    assert_eq!(u16::from_le_bytes([elf[16], elf[17]]), 2, "ET_EXEC");
    let s = String::from_utf8_lossy(&elf);
    for libc in ["printf", "snprintf", "sprintf"] {
        assert!(
            !s.contains(libc),
            "no libc `{libc}` for freestanding float printing"
        );
    }
}

#[test]
fn aarch64_musl_emits_the_same_object_as_gnu() {
    // musl and gnu share the code generation; only the link differs (static musl
    // vs dynamic glibc), so the relocatable object is byte-for-byte identical.
    let program = checked(r#""%d\n", 42;"#);
    let gnu = Arm64Linux::new(temp()).object(&program).unwrap();
    let musl = Arm64Linux::new_musl(temp()).object(&program).unwrap();
    assert_eq!(gnu, musl, "musl and gnu emit the same ELF object");
    assert_eq!(
        Arm64Linux::new_musl(temp()).name(),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(Arm64Linux::new(temp()).name(), "aarch64-unknown-linux-gnu");
}
