//! Target registration / scaffolding tests (host-independent — they only inspect
//! the emitted bytes or the error, never run a foreign binary).

use std::sync::atomic::{AtomicU32, Ordering};

use solomon::codegen::Codegen;
use solomon::codegen::arm64::Arm64Linux;
use solomon::codegen::x86_64::X64Linux;
use solomon::parser::parse;
use solomon::sema::check_program;

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
